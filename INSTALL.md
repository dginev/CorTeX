### System-level prerequisites:
 * A Postgres installation
 ```
   sudo apt-get install postgresql
 ```
 * A ZeroMQ installation and bindings
 ```
  sudo apt-get install libzmq3 libzmq3-dev
 ```

 * GnuPlot for classic plotting:
 ```
  sudo apt-get install libgd2-noxpm-dev libcairo2-dev gnuplot
 ```

* libarchive for dealing with complex directory jobs
```
 sudo apt-get install libarchive-dev
```

### Setting up postgresql:
 This is not normative, but the simplest (insecure!) approach is just:
 ```
 sudo emacs /etc/postgresql/9.1/main/pg_hba.conf
     change "local all all peer" to "local all all password"

 sudo -u postgres psql
     create database cortex_tester;
     create database cortex;

     create user cortex with password 'cortex';
     create user cortex_tester with password 'cortex_tester';

     grant all privileges on database cortex_tester to cortex_tester;
     grant all privileges on database cortex to cortex;
 ```

 This should evolve as we get nearer to production deploys...