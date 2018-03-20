### System-level prerequisites:
 * A Postgres installation, with the dev client-side bindings for diesel
```
  sudo apt-get install postgresql libpq-dev
```
 * A ZeroMQ installation and bindings
```
  sudo apt-get install libtool autoconf make pkg-config g++
  cd /tmp
  git clone --depth 1 -b stable https://github.com/jedisct1/libsodium.git
  cd libsodium
  ./autogen.sh
  ./configure --prefix=$HOME
  make
  make install
  cd ..
  wget https://github.com/zeromq/zeromq4-1/archive/v4.1.6.tar.gz
  tar zxf v4.1.6.tar.gz
  cd zeromq4-1-4.1.6
  ./autogen.sh
  ./configure --prefix=$HOME --with-libsodium
  make
  make install
cd ..
```

 * GnuPlot for classic plotting:
```
  sudo apt-get install libgd2-noxpm-dev libcairo2-dev gnuplot
```

* libarchive for dealing with complex directory jobs
```
 sudo apt-get install libarchive-dev
```

* A Redis caching server for mitigating DB load and maintaining temporary frontend state
```
  cd /tmp &&
  wget http://download.redis.io/redis-stable.tar.gz &&
  tar xvzf redis-stable.tar.gz &&
  cd redis-stable &&
  make &&
  make install
```

### Setting up postgresql:
 This is not normative, but the simplest (insecure!) approach is just changing the contents of `/etc/postgresql/9.10/main/pg_hba.conf` to:
 ```
    local all postgres peer
    local cortex cortex password
    local cortex_tester cortex_tester password
    host   all       all         127.0.0.1  255.255.255.255  trust
 ```

 and then introducing the accounts and databases via `sudo -u postgres psql` by typing in:
 ```
    create database cortex_tester;
    create database cortex;

    create user cortex with password 'cortex';
    create user cortex_tester with password 'cortex_tester';

    grant all privileges on database cortex_tester to cortex_tester;
    grant all privileges on database cortex to cortex;
 ```

 This should evolve as we get nearer to production deploys... Also, for now postgresql is expected on the default 5432 port.
 
### Setting up cortex database tables
 The `diesel` ORM allows us to run migrations to setup the database on a fresh installation. First, install the command-line tool:

```
   cargo install diesel_cli  --no-default-features --features postgres
```

Next, run the migrations, for both production and test databases:
```
   diesel migration run
   DATABASE_URL="postgres://cortex_tester:cortex_tester@localhost/cortex_tester" diesel migration run
```

### Optimized configuration for large datasets
 One of the problems that is experienced with arXiv, is that as we enter the tens-of-millions of rows for log messages, performance degrades very rapidly. One good solution to avoid that is to use a small threshold for VACUUM ANALYZE, given that the inserts are generally quick at the moment. [source of this trick](https://lob.com/blog/supercharge-your-postgresql-performance/) 
 ```
ALTER TABLE logs  
SET (autovacuum_enabled = true,
autovacuum_vacuum_scale_factor = 0.0002,
autovacuum_analyze_scale_factor = 0.0005,
autovacuum_analyze_threshold = 50,
autovacuum_vacuum_threshold = 50);

ALTER TABLE tasks  
SET (autovacuum_enabled = true,
autovacuum_vacuum_scale_factor = 0.0002,
autovacuum_analyze_scale_factor = 0.0005,
autovacuum_analyze_threshold = 50,
autovacuum_vacuum_threshold = 50);
```

Also, ensure you have the Postgres data directory on a sufficiently large disk. You may want 250GB available at a minimum for a LaTeXML run over arXiv. (See [here](https://github.com/dginev/CorTeX/issues/10) for details). Another consideration is running a newer version of Postgres, which may not be available in the default source list of some Linux server environments. The mathweb.org deployment currently uses Postgres 9.5 and upgraded from 9.1 roughly following an adaptation of [these upgrade guidelines](https://gist.github.com/tamoyal/2ea1fcdf99c819b4e07d).

### Development tips

Maintainer config for VSCode:
```
{
    "rust.rustup": {
        "toolchain": "nightly-x86_64-unknown-linux-gnu",
        "nightlyToolchain": "nightly-x86_64-unknown-linux-gnu"
    },
    "rust.mode": "rls",
    "rust.rls": {
        "useRustfmt": true
    },
    "rust.actionOnSave": "clippy",
    "editor.formatOnSave": true,
}
```

Recommended with `vscode-rust` plugin. Also for toolchain support, `clippy` and `rustfmt`. Also recommended RLS preview:
```
cargo install clippy
cargo install rustfmt
rustup component add rls-preview --toolchain nightly
```

Currently you need the nightly toolchain for developing with clippy and rls-preview, but we'll eventually port the repo to stable.
