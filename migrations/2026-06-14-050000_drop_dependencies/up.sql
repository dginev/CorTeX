-- Arm 12 rationalization: drop the dead `dependencies` table.
--
-- It was added in 2017 (`master`/`foundation` integer pairs) for an inter-service dependency feature
-- that was never built — no code reads or writes it (verified across `src/`), there are no foreign
-- keys to or from it, and CLAUDE.md lists it as dead. Keeping a never-used table only adds schema
-- noise (it shows up in `\dt`, `schema.rs`, and every schema diff). Real service-dependency
-- management (Arm 6, still a TODO) would design a fresh schema with proper service FKs rather than
-- revive these bare integer columns, so dropping it now loses nothing. Reversible (`down.sql`
-- recreates the original definition).
DROP TABLE dependencies;
