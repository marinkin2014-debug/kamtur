-- Two-phase доставка error-дайджестов.
--
-- Проблема, которую решаем:
--   1. Раньше `take_error_digest` помечал `notified_at` ДО отправки email.
--      Если SMTP падал — ошибки терялись навсегда.
--   2. Ошибки старше окна тоже терялись, если worker лежал.
--   3. Не было защиты от параллельных воркеров.
--
-- Решение: явные состояния + lease + attempts.
--   pending → claimed → sent
--                     → dead (исчерпаны попытки)

ALTER TABLE errors
    ADD COLUMN IF NOT EXISTS notification_state TEXT NOT NULL DEFAULT 'pending',
    ADD COLUMN IF NOT EXISTS notification_claimed_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS notification_attempts INT NOT NULL DEFAULT 0;

-- Бэкфилл: уже уведомлённые записи (notified_at NOT NULL) → 'sent'.
UPDATE errors
SET notification_state = 'sent'
WHERE notified_at IS NOT NULL AND notification_state = 'pending';

-- Constraint на состояния. Имя фиксировано — если в вашей БД уже есть
-- констрейнт с таким именем на другой таблице, переименуйте.
ALTER TABLE errors
    DROP CONSTRAINT IF EXISTS errors_notification_state_check;
ALTER TABLE errors
    ADD CONSTRAINT errors_notification_state_check
    CHECK (notification_state IN ('pending', 'claimed', 'sent', 'dead'));

-- Partial index для claim-запроса.
CREATE INDEX IF NOT EXISTS errors_claimable_idx
    ON errors (occurred_at)
    WHERE resolved = false
      AND notified_at IS NULL
      AND notification_state IN ('pending', 'claimed');