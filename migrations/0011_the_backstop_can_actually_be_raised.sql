-- The ceiling on the outer backstop happened to equal its own default, so it could not be
-- raised by even one notch.
--
-- The design of `worker_concurrency` is written down in the comment in 0001: **this is an
-- outer backstop, not a throttle**, the real throttling is left to the per-model semaphores,
-- and the backstop "must be clearly larger than the sum of the per-model limits, otherwise
-- the throttled jobs fill up the slots and starve the rest".
--
-- But the constraint was `BETWEEN 1 AND 32` while the default was also 32 -- **the constraint
-- blocked its own design**. It got there in two steps: the constraint was written when the
-- default was still 4 (a ceiling of 32 was eight times the headroom), then the default went
-- from 4 up to 32 and nobody went back to look at that constraint. The validation on the Rust
-- side did get changed to `1..=256`, so the two sides disagreed: fill in any value between 33
-- and 256 on the settings page and Rust lets it through while the database rejects it, and
-- what the user sees is a CHECK constraint error rather than "out of range".
--
-- The ceiling is aligned with Rust at 256.
--
-- **The default goes up to 64**, because that line "otherwise it starves the rest" is
-- something that has actually been observed: in one extraction over 219 documents, 32
-- `extract_document` jobs filled the slots and `embed_ontology` sat in queued unable to get
-- in. 32 against a model limit of 10 is only 3.2x, so extraction turning up at all gums up
-- the queue.
--
-- Why 64 and not more: a job parked on a semaphore is nearly free (it holds no connection --
-- there is no transaction spanning an await on the extraction path), but **before it ever
-- gets to the semaphore** each chunk still does two things that touch the database --
-- one `extract_epoch` query, plus a vector search over all relations and classes when the
-- ontology is large. Neither of those is bounded by the model semaphores. So the concurrency
-- number directly decides how many searches slam into the database at once, and the pool is
-- 32. 64 is 2x oversubscription, still on the "gets slower" rather than the "times out" side;
-- going higher than that should first move the permit acquisition to the very front of each
-- unit of work, so that a slot really is close to free -- that is a different change.
--
-- Only change the ones nobody has touched (value still equal to the old default of 32);
-- whoever tuned it by hand has their choice respected -- the same policy as the 4 → 32
-- change back then.

ALTER TABLE deployment_settings
    DROP CONSTRAINT IF EXISTS deployment_settings_worker_concurrency_check;

ALTER TABLE deployment_settings
    ADD CONSTRAINT deployment_settings_worker_concurrency_check
    CHECK (worker_concurrency BETWEEN 1 AND 256);

ALTER TABLE deployment_settings
    ALTER COLUMN worker_concurrency SET DEFAULT 64;

UPDATE deployment_settings SET worker_concurrency = 64 WHERE worker_concurrency = 32;
