-- Sandbox layout: a sandbox is a VIEW of a root corpus, never a second corpus at the same location.
--
-- Sandboxes reference their parent's `path` in place (no source copy), so `path` alone stopped being
-- an identity: on 2026-10-05 `extend_corpora /data/arxmliv/ 2609` resolved that path to
-- `sandbox-arxiv-2605` (an unordered `.first()` over three rows) and imported a whole month into it.
--
-- 1) One ROOT corpus per path, so a path lookup is deterministic. Sandboxes are exempt: they share
--    their parent's path by design.
CREATE UNIQUE INDEX corpora_root_path_key ON corpora (path) WHERE parent_corpus_id IS NULL;

-- 2) A sandbox hangs off a real parent and is deleted with it (its tasks point into the parent's
--    tree). No orphan sweep on purpose: a violating database fails this migration loudly rather
--    than silently losing corpora.
ALTER TABLE corpora ADD CONSTRAINT corpora_parent_corpus_id_fkey
  FOREIGN KEY (parent_corpus_id) REFERENCES corpora(id) ON DELETE CASCADE;
