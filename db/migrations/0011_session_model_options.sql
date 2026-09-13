-- Model options belong to the conversation so every resumed turn runs with
-- the choices the reader made when it started.
ALTER TABLE sessions ADD COLUMN reasoning_effort TEXT;
ALTER TABLE sessions ADD COLUMN service_tier TEXT;
