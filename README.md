![CorTeX Framework](./public/img/logo.jpg) Framework
======

**A general purpose processing framework for corpora of scientific documents**

[![CI](https://github.com/dginev/CorTeX/actions/workflows/CI.yml/badge.svg)](https://github.com/dginev/CorTeX/actions/workflows/CI.yml) [![API Documentation](https://img.shields.io/badge/docs-API-blue.svg)](http://dginev.github.io/CorTeX/cortex/index.html) [![License](https://img.shields.io/badge/license-MIT-blue.svg)](https://raw.githubusercontent.com/dginev/CorTeX/master/LICENSE) ![version](https://img.shields.io/badge/version-0.5.0-orange.svg)


**News**:
  - **2019:** [![arXMLiv Homepage](https://img.shields.io/badge/web-corpora.mathweb.org-red.svg?style=flat&label=https&colorB=d33847)](https://corpora.mathweb.org) The CorTeX framework is recurringly converting >1.5 million articles from arXiv.org
  - **2018:** [![SIGMathLing resources](https://img.shields.io/badge/web-sigmathling.kwarc.info-blue.svg?style=flat&label=https&colorB=3895d3)](https://sigmathling.kwarc.info/resources/) First datasets produced by CorTeX are now available for reuse via the SIGMathLing interest group

**Features**:
 - [x] Safe and speedy Rust implementation
 - [x] Distributed processing and streaming data transfers via **ZeroMQ**
 - [x] Backend support for Document (via FileSystem) and Task (via PostgreSQL **18+**) provenance.
 - [x] Representation-aware and -independent (TeX, HTML+RDFa, ePub, TEI, JATS, ...)
 - [x] Powerful workflow management and development support through the CorTeX web interface
 - [x] Supports multi-corpora multi-service installations
 - [x] Centralized storage, with distributed computing, motivated to enable collaborations across institutional and national borders.
 - [x] Routinely tested on 1 million scientific TeX papers from arXiv.org
 - [x] **Self-installing**: `cortex init` / `doctor` (embedded migrations, runtime config, health check)
 - [x] **Agent-first & symmetric**: every capability on three surfaces — web UI, agent JSON API (OpenAPI at `/api/docs`), and the scriptable `cortex` CLI — from one shared backend, so all see the same live + historical state
 - [x] **Observability**: background-job health, `/health` + Prometheus `/metrics`, an audit log of every admin action
 - [x] **Local authentication**: passkeys (WebAuthn) + admin tokens, server-side sessions
 - [ ] Annotations backend and workflow (TODO)
 - [ ] Automatic dependency management of registered Services (TODO)

**Getting started** (full steps in [INSTALL.md](INSTALL.md); day-to-day operation in [MANUAL.md](MANUAL.md)):

```bash
sudo apt install -y postgresql libpq-dev libzmq3-dev libsodium-dev pkg-config
cargo run --bin cortex -- init                                  # migrate + scaffold config
cargo run --bin cortex -- set-admin-token --generate --owner me # first admin credential
cargo run --bin cortex -- doctor                                # verify: => healthy
cargo run --bin frontend                                        # serves http://127.0.0.1:8000
```

**Admin token & secrets**: writes and the `/admin` sign-in are gated by an admin token (sent as an
`X-Cortex-Token` header or `?token=` query) mapped to an
owner (`rerun_tokens`). Tokens live in a **gitignored token file** — the default is `config.json` in
the working directory (the tracked `config.default.json` is a *demo template only*, never your real
token), scaffolded/managed by `cortex set-admin-token`. To keep a **production** token out of the
repository entirely, put it in a JSON file outside the tree and point `CORTEX_AUTH_FILE` at it (it
overrides the in-repo config):

```bash
# /etc/cortex/config.json  (chmod 600, owned by the service user — never in git)
{ "rerun_tokens": { "<your-token>": "<owner>" } }
# then, in the service environment (e.g. /etc/cortex/frontend.env):
CORTEX_AUTH_FILE=/etc/cortex/config.json
```

Humans can also sign in with passkeys (WebAuthn); the token is the agent + break-glass credential.

**History**:
 * Originally motivated by the desire to process any **Cor**-pus of **TeX** documents.
 * Rust reimplementation of the original Perl [CorTeX](https://github.com/dginev/deprecated-CorTeX) stack.
 * Builds on the expertise developed during the [arXMLiv project](https://kwarc.info/projects/arXMLiv/) at Jacobs University.
 * In particular, CorTeX is a successor to the [build system](https://link.springer.com/article/10.1007/s11786-010-0024-7) originally developed by Heinrich Stamerjohanns.
 * The messaging conventions are adopted from work on standardizing [LaTeXML](http://dlmf.nist.gov/LaTeXML)'s log reports with Bruce Miller.

CorTeX is being **productized** from an admin-only prototype into a self-installing, agent-first +
human-first application — an active sprint (see [docs/PRODUCTIZING_PLAN.md](docs/PRODUCTIZING_PLAN.md)),
with a public preview in preparation. It now self-installs and ships a complete
[operator manual](MANUAL.md) and agent API, though some hardening/rationalization is still in flight.
For details consult the [Installation](INSTALL.md) instructions and the [Manual](MANUAL.md). Please
feel invited to open an issue with a description of your target workflow!
