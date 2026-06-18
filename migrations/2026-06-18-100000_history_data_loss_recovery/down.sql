-- Remove the synthetic recovered arXMLiv history (exact-match on the recovery description,
-- scoped to arXiv / tex_to_html), leaving any real runs untouched.
DELETE FROM historical_runs hr
USING corpora c, services s
WHERE hr.corpus_id = c.id AND hr.service_id = s.id
  AND c.name = 'arXiv' AND s.name = 'tex_to_html'
  AND hr.description = 'reconstructed arXMLiv build — synthetic approximation from arxmliv_history_2022.svg (history_data_loss_recovery)';
