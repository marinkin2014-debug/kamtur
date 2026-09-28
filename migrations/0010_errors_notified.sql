-- Поле для error digest: помечает ошибки, уже отправленные в дайджесте.
-- Позволяет worker'у группировать ошибки и отправлять одно письмо раз в час.
ALTER TABLE errors ADD COLUMN IF NOT EXISTS notified_at TIMESTAMPTZ;

-- Частичный индекс для быстрого поиска неотправленных ошибок.
CREATE INDEX IF NOT EXISTS errors_digest_pending
    ON errors (occurred_at)
    WHERE notified_at IS NULL AND resolved = false;
