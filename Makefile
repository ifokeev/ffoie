.PHONY: docker-up docker-down soak musl-build

# Start the full stack (chat server + wasm engine served via nginx).
# Override FFOIE_WEB_PORT if 8081 is taken on your host.
#   chat-server      → http://localhost:8080
#   engine-web (wasm) → http://localhost:${FFOIE_WEB_PORT:-8081}
docker-up:
	docker compose up --build -d
	@echo ""
	@echo "  chat-server : http://localhost:8080/healthz"
	@echo "  engine-web  : http://localhost:$${FFOIE_WEB_PORT:-8081}"

# Stop and remove both containers
docker-down:
	docker compose down --remove-orphans

# Run the 1000-connection 5-minute soak test (requires server running on :8080)
# Containerized: docker run --rm --network host -v $(PWD)/scripts:/scripts grafana/k6:latest run /scripts/soak/chat-soak.js
soak:
	k6 run --vus 1000 --duration 5m scripts/soak/chat-soak.js

# Build a statically-linked Linux binary (requires musl-tools on Linux)
musl-build:
	cargo build --release --target x86_64-unknown-linux-musl -p ffoie-chat-server
