extern crate core;

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::time::Duration;

use bincode::{Decode, Encode};
use clap::{ArgEnum, Args, Subcommand};
use clap::Parser;
use env_logger::Env;
use rdkafka::{ClientConfig, Message, Offset, TopicPartitionList};
use rdkafka::config::RDKafkaLogLevel;
use rdkafka::consumer::{BaseConsumer, Consumer};
use rdkafka::error::{KafkaError, KafkaResult, RDKafkaErrorCode};
use rdkafka::message::{BorrowedHeaders, Headers, OwnedHeaders};
use rdkafka::producer::{BaseProducer, BaseRecord, Producer};
use rdkafka::util::Timeout;

#[derive(Debug, Encode)]
struct SerHeader<'a> {
    name: &'a str,
    value: &'a [u8],
}

#[derive(Debug, Encode)]
struct SerMessage<'a> {
    partition: i32,
    key: &'a [u8],
    value: &'a [u8],
    headers: Vec<SerHeader<'a>>,
}

#[derive(Debug, Decode)]
struct DeserHeader {
    name: String,
    value: Vec<u8>,
}

#[derive(Debug, Decode)]
struct DeserMessage {
    partition: i32,
    key: Vec<u8>,
    value: Vec<u8>,
    headers: Vec<DeserHeader>,
}

#[derive(Parser, Debug)]
#[clap(name = "kafka dumper", version)]
#[clap(about = "Dump or Restore a a Kafka topic", long_about = None)]
#[clap(arg_required_else_help = true)]
struct Cli {
    /// Kafka brokers list in kafka format
    #[clap(long, short = 'b', value_parser, default_value = "localhost:9092")]
    brokers: String,

    /// Kafka broker security protocol
    #[clap(long, value_parser, default_value = "plaintext")]
    security_protocol: Protocol,

    /// Client ssl keystore location
    #[clap(long, value_parser)]
    ssl_keystore_location: Option<String>,

    /// Client ssl keystore password
    #[clap(long, value_parser)]
    ssl_keystore_password: Option<String>,

    /// Kafka topic to read from or write to
    #[clap(long, short = 't', value_parser)]
    topic: String,

    /// Dump file to write to or read from
    #[clap(long, short = 'f', value_parser, value_hint = clap::ValueHint::FilePath)]
    file: std::path::PathBuf,

    #[clap(subcommand)]
    command: Commands,
}

#[derive(Clone, Debug, Subcommand, ArgEnum)]
enum Protocol {
    Plaintext,
    Ssl,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Dump topic to file
    Dump,
    /// Restore file to topic
    Restore(Restore),
}

#[derive(Debug, Args)]
#[clap(args_conflicts_with_subcommands = true)]
struct Restore {
    #[clap(long, short = 'p', arg_enum, value_parser, default_value = "default-hash-key")]
    partitioning_strategy: PartitioningStrategy,
}

#[derive(Clone, Debug, Subcommand, ArgEnum)]
enum PartitioningStrategy {
    OriginPartition,
    DefaultHashKey,
}

fn main() {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();
    let args: Cli = Cli::parse();
    log::debug!("launched with args {:?}", args);

    match &args.command {
        Commands::Dump => consume(&args),
        Commands::Restore(restore_opts) => produce(&args, restore_opts.partitioning_strategy.clone()),
    };
}

fn produce(cli: &Cli, partitioning_strategy: PartitioningStrategy) {
    let file = File::open(&cli.file).unwrap();
    let file = snap::read::FrameDecoder::new(file);
    let mut file = BufReader::new(file);
    let mut size_buff = [0u8; 2];
    let mut data_buff = [0u8; u16::MAX as usize];


    let producer: BaseProducer = client_config(&cli)
        .set("linger.ms", "100")
        .set("request.required.acks", "all")
        .set("enable.idempotence", "true")
        .create()
        .expect("invalid producer config");

    let mut count = 0;
    while let Ok(()) = file.read_exact(&mut size_buff) {
        let size = ((size_buff[0] as u16) << 8 | (size_buff[1] as u16)) as usize;
        file.read_exact(&mut data_buff[..size]).expect("read data");

        let (payload_obj, _): (DeserMessage, usize) = bincode::decode_from_slice(&data_buff[..size], bincode::config::standard()).expect("zut");

        while let Err((err, _)) = send(&cli.topic, &producer, &payload_obj, &partitioning_strategy) {
            if let KafkaError::MessageProduction(RDKafkaErrorCode::QueueFull) = err {
                producer.poll(Duration::from_secs(5));
                continue;
            }
            panic!("{}", err);
        }
        count += 1;
    }

    producer.flush(Duration::from_secs(1200));

    log::info!("produced {} messages", count);
}

fn client_config(cli: &Cli) -> ClientConfig {
    let mut client_config = ClientConfig::new();
    client_config
        .set_log_level(RDKafkaLogLevel::Debug)
        .set("bootstrap.servers", &cli.brokers);
    match &cli.security_protocol {
        Protocol::Plaintext => client_config.set("security.protocol", "plaintext"),
        Protocol::Ssl => {
            client_config.set("security.protocol", "SSL")
                .set("enable.ssl.certificate.verification", "false")
                .set("ssl.keystore.location", cli.ssl_keystore_location.as_ref().expect("ssl-keystore-location is missing"))
                .set("ssl.keystore.password", cli.ssl_keystore_password.as_ref().expect("ssl-keystore-password is missing"))
        }
    };
    client_config
}

fn send<'a>(topic: &'a str, producer: &BaseProducer, payload_obj: &'a DeserMessage, partitioning_strategy: &PartitioningStrategy) -> Result<(), (KafkaError, BaseRecord<'a, [u8], [u8]>)> {
    let mut headers = OwnedHeaders::new();
    let vec = &payload_obj.headers[..];
    for header in vec.iter() {
        headers = headers.add(&header.name, &header.value);
    }

    let partition = payload_obj.partition;

    let record = BaseRecord::to(topic)
        .key(&payload_obj.key[..])
        .payload(&payload_obj.value[..])
        .headers(headers);

    let record = match partitioning_strategy {
        PartitioningStrategy::OriginPartition => record.partition(partition),
        PartitioningStrategy::DefaultHashKey => record
    };

    producer.send(record)
}


fn consume(cli: &Cli) {
    let consumer: BaseConsumer = client_config(&cli)
        .set("group.id", "_unused_group_id")
        .set("enable.auto.commit", "false")
        .set("auto.offset.reset", "earliest")
        .create()
        .expect("invalid consumer config");

    let list = fetch_topic_partitions(&consumer, &cli.topic).expect("topicList");
    if list.count() < 1 {
        log::info!("no partitions");
        return;
    }
    consumer
        .assign(&list)
        .expect("topic subscribe failed");

    let file = File::create(&cli.file).unwrap();
    let file = snap::write::FrameEncoder::new(file);
    let mut file = BufWriter::new(file);
    let mut buffer = [0u8; 2 + u16::MAX as usize];
    let mut count = 0;
    loop {
        let msg = consumer.poll(Timeout::After(Duration::from_secs(1)));
        match msg {
            None => {
                log::info!("consumed {} messages", count);
                return;
            }
            Some(msg) => {
                let msg = msg.expect("borrowedmessage");
                let key = msg.key().unwrap();
                let value = msg.payload().unwrap();
                let partition = msg.partition();
                let headers = match msg.headers() {
                    None => vec![],
                    Some(headers) => IterHeaders::new(headers).map(|(name, value)| SerHeader { name, value }).collect()
                };
                let message = SerMessage { partition, key, value, headers };

                let written = bincode::encode_into_slice(&message, &mut buffer[2..], bincode::config::standard()).expect("TODO: panic message");
                buffer[0] = ((written & 0xFF00) >> 8) as u8;
                buffer[1] = (written & 0x00FF) as u8;

                file.write(&buffer[..(written + 2)]).expect("fail write");
                count += 1;
            }
        }
    }
}

fn fetch_topic_partitions(consumer: &BaseConsumer, topic: &str) -> KafkaResult<TopicPartitionList> {
    TopicPartitionList::from_topic_map(
        &consumer.fetch_metadata(Some(topic), Timeout::After(Duration::from_secs(1)))?.topics().iter()
            .flat_map(|tm| tm.partitions().iter().map(|pm| ((tm.name().to_string(), pm.id()), Offset::Beginning)))
            .collect()
    )
}


pub struct IterHeaders<'a> {
    headers: &'a BorrowedHeaders,
    next: usize,
    count: usize,
}

impl<'a> IterHeaders<'a> {
    fn new(headers: &'a BorrowedHeaders) -> IterHeaders<'a> {
        let count = headers.count();
        IterHeaders { headers, count, next: 0 }
    }
}

impl<'a> Iterator for IterHeaders<'a> {
    type Item = (&'a str, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let next = self.next;
        if next == self.count {
            None
        } else {
            self.next += 1;
            self.headers.get(next)
        }
    }
}