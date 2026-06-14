-- Drop the TODO leasing partial index, restoring the pre-migration index set.
drop index if exists todo_index;
