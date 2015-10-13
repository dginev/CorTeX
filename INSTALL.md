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
 
 One of the problems that is experienced with arXiv, is that as we enter the tens-of-millions of rows for log messages, performance degrades very rapidly. One good solution to avoid that is to use a small threshold for VACUUM ANALYZE, given that the inserts are generally quick at the moment. [source of this trick](https://lob.com/blog/supercharge-your-postgresql-performance/) 
 ```
ALTER TABLE logs  
SET (autovacuum_vacuum_scale_factor = 0.0);
ALTER TABLE logs  
SET (autovacuum_vacuum_threshold = 5000);
ALTER TABLE logs  
SET (autovacuum_analyze_scale_factor = 0.0);
ALTER TABLE logs  
SET (autovacuum_vacuum_threshold = 5000);  

ALTER TABLE tasks  
SET (autovacuum_vacuum_scale_factor = 0.0);
ALTER TABLE tasks  
SET (autovacuum_vacuum_threshold = 5000);
ALTER TABLE tasks  
SET (autovacuum_analyze_scale_factor = 0.0);
ALTER TABLE tasks  
SET (autovacuum_vacuum_threshold = 5000);  
```

Also, ensure you have the Postgres data directory on a sufficiently large disk. You may want 250GB available at a minimum for a LaTeXML run over arXiv. (See [here](https://github.com/dginev/CorTeX/issues/10) for details)
