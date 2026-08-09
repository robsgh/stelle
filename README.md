# Stelle

Stelle is a self-hostable and customizable dashboard/start screen for homelabbers. It combines responsive link cards with sandboxed, server-side Luau widgets in a single, minimal container.

![MVP](https://img.shields.io/badge/status-MVP-8b5cf6)

## Quick start

Build and run the image:

```sh
docker build -t stelle .
docker run --rm --name stelle -p 8080:8080 stelle
```

Open <http://localhost:8080>. The bundled dashboard contains a Luau-powered GitHub statistics card plus links to YouTube and the configured Proxmox host.

You can also run it behind Traefik. The Compose deployment reads its private routing values from an untracked `.env` file and expects the configured external Docker network to already exist:

```sh
docker compose up --build
```

Stelle has no built-in authentication in this MVP. Keep it on a trusted network or place it behind an authenticated reverse proxy.

## Configure the dashboard

Configuration is loaded once from `/config/dashboard.yaml`. To customize it, edit a local `config` directory and mount it read-only:

```sh
docker run --rm --name stelle -p 8080:8080 \
  -v "$PWD/config:/config:ro" stelle
```

Restart the container after changing configuration or scripts. `STELLE_CONFIG` can point to a different YAML file inside the container.
Stelle listens on port `8080` by default; set `STELLE_PORT` to override it.

A configuration only needs a `widgets` list. Optional top-level settings are `title` (`Stelle`), `subtitle` (`Your homelab, at a glance.`), `theme` (`system`, `light`, or `dark`), and `accent` (`#8b5cf6`).

### Link widgets

Link cards support internet services, internal DNS names, IP addresses, and non-standard ports. The browser loads each site's conventional `/favicon.ico` directly.

```yaml
widgets:
  - type: link
    label: Router
    description: Network administration
    url: https://192.168.1.1:8443
    accent: "#38bdf8"
```

### Luau widgets

Luau widgets return a constrained statistics model. Each widget must explicitly allow every network origin it contacts:

```yaml
widgets:
  - type: lua
    id: repository
    script: widgets/github-stats.luau
    network_allow:
      - https://api.github.com
    settings:
      repository: robsgh/stelle
```

The script receives these globals:

- `settings`: read-only values from the widget configuration
- `http.get(url, headers?)`: an allowlisted HTTP GET returning `{ status, body }`
- `json.decode(value)` and `json.encode(value)`
- `log(message)`: server-side informational logging

It must return:

```lua
return {
    title = "Widget title",
    subtitle = "Optional detail",
    href = "https://example.com",
    metrics = {
        { label = "Healthy", value = 12 },
        { label = "Offline", value = 1 }
    },
    fetched_at = os.date("!%Y-%m-%dT%H:%M:%SZ")
}
```

Every refresh uses a new sandbox with memory, execution-time, response-size, and outbound-origin limits. Widgets have no persistence, filesystem, process, environment, secret, or raw-socket API.

## Local development

Start the backend, pointing it at the repository assets:

```sh
STELLE_CONFIG="$PWD/config/dashboard.yaml" \
STELLE_STATIC_DIR="$PWD/frontend/build" \
cargo run
```

For frontend hot reload, run `npm install && npm run dev` inside `frontend/`; Vite proxies API requests to port 8080.

Useful checks:

```sh
cargo test
cd frontend && npm run check && npm run build
```

## HTTP API

- `GET /api/dashboard` returns theme and public widget metadata.
- `POST /api/widgets/{id}/refresh` executes a Luau widget.
- `GET /healthz` is the container health endpoint.

Stelle is licensed under the [MIT License](LICENSE).
