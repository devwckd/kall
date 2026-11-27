# Hosting kall at home

kall-server is a single UDP port, `7744`. The server does three jobs itself:
- it gets and renews a Let's Encrypt certificate (DNS-01 through Cloudflare), so no web server or open TCP port is needed;
- it keeps `kall.<your domain>` pointing at your home IP;
- it stores everything in one Docker volume.

## 1. Cloudflare token

Create the token in Cloudflare → My Profile → API Tokens → Create Token → **Custom token**:
- **Permissions:**
  - `Zone` → `DNS` → `Edit`
  - `Zone` → `Zone` → `Read`
- **Zone Resources:** Include → Specific zone → your domain.

You don't need to create the DNS record. The server makes `kall.<domain>` as a **DNS-only (grey cloud)** A record.
If you make it by hand, keep it grey: Cloudflare's proxy can't carry QUIC to a custom port.

## 2. Router

Forward **UDP 7744** to the Linux box. TCP isn't needed.

## 3. Run it

```sh
cd deploy
cp .env.example .env    # fill in KALL_DOMAIN and CF_API_TOKEN; leave KALL_ACME_STAGING=1 at first
docker compose up -d --build
docker compose logs -f kall
```

With staging on, the log should show `got a certificate for kall.<domain>`. Staging certificates aren't trusted by clients, so this step only proves the token and DNS work.
Then set `KALL_ACME_STAGING=0` and run `docker compose up -d`. The production certificate is saved in the volume, then renewed 30 days before it expires and swapped in without a restart.

The first start also prints the **owner invite**:

```
no accounts yet: register the owner account with invite code  xxxxxxxxxx
```

In the kall app, choose **Create account**, enter `kall.<domain>` as the server, and paste that code.
After that, make invites for friends in the app (Settings → Invites), or from the shell:

```sh
docker compose exec kall kall-server invite --uses 1 --hours 168
```

## Useful commands

```sh
docker compose exec kall kall-server ddns    # update the A record now, print the IP
docker compose exec kall kall-server cert    # check/renew the certificate now
docker compose down                          # stop (data stays in the kall-data volume)
```

## Without a domain (LAN or testing)

Leave `KALL_DOMAIN` and `CF_API_TOKEN` empty. The server then uses a self-signed certificate.
Clients connecting by **IP address** pin its fingerprint the first time.
A client connecting by host name refuses a self-signed certificate unless that client runs with `KALL_TRUST_SELF_SIGNED=1`.

## Notes

- **Firewall:** if the box runs one, allow `7744/udp`.
- **IPv6:** to also publish an AAAA record, set `KALL_DDNS_IPV6=1`. The container also needs IPv6: `network_mode: host`, or Docker's IPv6 support, plus `KALL_BIND=[::]:7744`.
- **CGNAT:** if your ISP puts you behind CGNAT, there's no forwardable public IPv4. That needs IPv6 or a relay VPS.
