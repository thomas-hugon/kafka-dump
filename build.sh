docker build -t "kafka-dump:latest" . && container_id=$(docker create "kafka-dump:latest") && docker cp "${container_id}:/kafka-dump" "./kafka-dump"
