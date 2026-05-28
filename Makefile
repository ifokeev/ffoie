.PHONY: docker-up docker-down soak musl-build

# Start the chat server container (rebuilds image if Dockerfile changed)
docker-up:
	docker compose up --build -d

# Stop and remove the chat server container
docker-down:
	docker compose down

# Run the 1000-connection 5-minute soak test (requires server running on :8080)
# Containerized: docker run --rm --network host -v $(PWD)/scripts:/scripts grafana/k6:latest run /scripts/soak/chat-soak.js
soak:
	k6 run --vus 1000 --duration 5m scripts/soak/chat-soak.js

# Build a statically-linked Linux binary (requires musl-tools on Linux)
musl-build:
	cargo build --release --target x86_64-unknown-linux-musl -p ffoie-chat-server
