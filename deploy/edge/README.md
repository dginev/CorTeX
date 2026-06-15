# Edge setup (Vultr `ar5iv-edit-1`, public anchor for `*.latexml.rs`)

The edge already runs Caddy (TLS for `latexml.rs` + the ar5iv editor) behind one
Anubis bot wall. Exposing cortex at `corpora.latexml.rs` reuses that Anubis.

## 1. Reverse-tunnel ingress user (locked down)

```bash
useradd --system --create-home --shell /usr/sbin/nologin cortextunnel
install -d -m700 -o cortextunnel -g cortextunnel /home/cortextunnel/.ssh
# Only this one reverse forward, nothing else:
printf 'restrict,port-forwarding,permitlisten="127.0.0.1:8000" %s\n' \
  "$(cat cortex_tunnel_ed25519.pub)" > /home/cortextunnel/.ssh/authorized_keys
chown cortextunnel:cortextunnel /home/cortextunnel/.ssh/authorized_keys
chmod 600 /home/cortextunnel/.ssh/authorized_keys
```

## 2. Share the Anubis (in `ar5iv-editor` repo: `deploy/docker-compose.yml`)

Already committed to `ar5iv-editor` `main`:
- Anubis `TARGET: http://host.docker.internal:8181` + `extra_hosts: host.docker.internal:host-gateway`
- editor published on host loopback: `ports: ["127.0.0.1:3000:3000"]`

`cd /opt/ar5iv-editor/deploy && docker compose up -d` (recreates anubis + editor; seconds).

## 3. Caddy

Append `corpora.caddy` to `/etc/caddy/Caddyfile`, add the ufw rule, validate, reload:

```bash
ufw allow from 172.16.0.0/12 to any port 8181 proto tcp comment 'anubis->cortex/editor router'
caddy validate --adapter caddyfile --config /etc/caddy/Caddyfile
systemctl reload caddy
```

## Verify

```bash
curl -s https://corpora.latexml.rs/healthz            # 200 cortex JSON, NO anubis cookie
curl -sI https://corpora.latexml.rs/   | grep anubis  # set-cookie techaro.lol-anubis-* (HTML walled)
curl -s -o /dev/null -w '%{http_code}\n' https://corpora.latexml.rs/about  # 404 = reached cortex
                                                                           #       (Host preserved, not the editor)
```
