-- Widen tasks.entry from varchar(200) (KNOWN_ISSUES R-2). `entry` is the absolute path to a
-- document's source archive; a long path (hostile/long arXiv entries, deep directory trees) exceeding
-- 200 chars currently makes the task INSERT error ("value too long"), so the document is silently lost
-- to processing. 4096 covers a Linux PATH_MAX absolute path.
--
-- Increasing a varchar length limit is a catalog-only change in Postgres — no table rewrite and no
-- index rebuild (the stored values and the seven indexes over `entry` are untouched) — so this is
-- safe to run on the large `tasks` table without a maintenance window.
ALTER TABLE tasks ALTER COLUMN entry TYPE varchar(4096);
