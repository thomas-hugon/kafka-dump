docker build -t "kafka-dmp:latest" . && container_id=$(docker create "kafka-dmp:latest") && docker cp "${container_id}:/kafka-dmp" "./kafka-dmp"
