-- Your SQL goes here
-- This migration creates a table to store historical task data.
CREATE TABLE historical_tasks (
    id BIGSERIAL PRIMARY KEY,
    task_id BIGINT NOT NULL,
    status INTEGER NOT NULL,
    saved_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Add a foreign key constraint to link historical tasks to tasks
ALTER TABLE historical_tasks
ADD CONSTRAINT fk_historical_tasks_tasks
FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE;

CREATE INDEX idx_historical_tasks_task_id ON historical_tasks(task_id);
CREATE INDEX idx_historical_tasks_status ON historical_tasks(status);
