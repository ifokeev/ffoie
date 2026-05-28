.PHONY: docker-up docker-down soak musl-build

# Start the full stack (chat server + wasm engine served via nginx).
# Host ports are unique by default; override FFOIE_CHAT_PORT / FFOIE_WEB_PORT
# if they clash with other local services.
#   chat-server       → http://localhost:${FFOIE_CHAT_PORT:-47820}
#   engine-web (wasm) → http://localhost:${FFOIE_WEB_PORT:-47821}
docker-up:
	docker compose up --build -d
	@echo ""
	@echo "  chat-server : http://localhost:$${FFOIE_CHAT_PORT:-47820}/healthz"
	@echo "  engine-web  : http://localhost:$${FFOIE_WEB_PORT:-47821}"

# Stop and remove both containers
docker-down:
	docker compose down --remove-orphans

# Run the 1000-connection 5-minute soak test.
# Requires the chat server reachable; point the script at the host port with
# WS_URL (defaults inside the script to ws://localhost:47820/ws).
# Containerized: docker run --rm --network host -v $(PWD)/scripts:/scripts grafana/k6:latest run /scripts/soak/chat-soak.js
soak:
	k6 run --vus 1000 --duration 5m scripts/soak/chat-soak.js

# Build a statically-linked Linux binary (requires musl-tools on Linux)
musl-build:
	cargo build --release --target x86_64-unknown-linux-musl -p ffoie-chat-server
