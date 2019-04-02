drop index log_fatals_task_id;
create index permissive_log_fatals_task_id on log_fatals(task_id);