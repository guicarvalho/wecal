-- WeCal: seed estruturado (mock) — roda logo após as migrations
-- Representa a agenda de um casal: ele (Google/trabalho) x ela (Apple/pessoal).

INSERT INTO wecal_events (provider, title, description, start_at, end_at, is_private) VALUES
  -- Google (ele, trabalho)
  ('google', 'Daily de produto', 'Alinhamento com o time de produto', now() + interval '1 day' + interval '9 hours',  now() + interval '1 day' + interval '9 hours 30 minute', false),
  ('google', 'Entrevista técnica', 'Candidato sênior — sigiloso',          now() + interval '2 days' + interval '14 hours', now() + interval '2 days' + interval '15 hours', true),
  -- Apple (ela, pessoal)
  ('apple', 'Pediatra', 'Levar o Theo',                                    now() + interval '1 day' + interval '9 hours 15 minute', now() + interval '1 day' + interval '10 hours', false),
  ('apple', 'Jantar com a Ana', 'Restaurante italiano no centro',          now() + interval '3 days' + interval '19 hours', now() + interval '3 days' + interval '21 hours', false),
  -- Conflito intencional para demonstrar is_conflict
  ('apple', 'Reunião com a contadora', 'Fechamento mensal',               now() + interval '2 days' + interval '14 hours 30 minute', now() + interval '2 days' + interval '15 hours 30 minute', false)
ON CONFLICT DO NOTHING;
