FROM clux/muslrust:latest as kafka-dmp-builder

ADD ./Cargo.toml /volume/
RUN cargo fetch
RUN cargo build --release --target x86_64-unknown-linux-musl --bin kafka-dump; mv ./Cargo.lock /tmp/Cargo.lock ; exit 0


FROM kafka-dmp-builder as kafka-dmp

ADD ./src/* /volume/src/
ADD ./Cargo.toml /volume/
RUN mv -f /tmp/Cargo.lock ./Cargo.lock
RUN cargo build --release --target x86_64-unknown-linux-musl --bin kafka-dump


FROM busybox:latest

COPY --from=kafka-dmp /volume/target/x86_64-unknown-linux-musl/release/kafka-dump /kafka-dump
ENTRYPOINT ["/kafka-dump"]