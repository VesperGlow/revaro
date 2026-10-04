-- Remove the display-only task projection; upload sessions and parts stay intact.
DROP TABLE IF EXISTS task_files;
DROP TABLE IF EXISTS tasks;
