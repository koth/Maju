/** Entry point: start the CodeBuddy proxy server. */
import { createApp } from './server.js';
import { loadConfig } from './config.js';
import { logger, setLevel } from './logger.js';

async function main(): Promise<void> {
  const cfg = loadConfig();
  setLevel(cfg.logLevel);
  logger.info('starting codebuddy proxy on %s:%d (defaultModel=%s, auth=%s, maxSessions=8)', cfg.host, cfg.port, cfg.defaultModel, cfg.apiKey ? 'enabled' : 'disabled');
  const { app, pool } = createApp(cfg);
  const server = app.listen(cfg.port, cfg.host, () => {
    logger.info('codebuddy proxy listening at http://%s:%d/v1', cfg.host, cfg.port);
  });
  const shutdown = async (sig: string): Promise<void> => {
    logger.info('received %s, draining session pool and shutting down', sig);
    await pool.drain();
    server.close(() => process.exit(0));
  };
  process.on('SIGINT', () => void shutdown('SIGINT'));
  process.on('SIGTERM', () => void shutdown('SIGTERM'));
}

// Swallow the transport-teardown race that the SDK throws when a query is
// interrupted at the tool boundary (CLI exits while a pending control
// response write is queued).
process.on('unhandledRejection', (reason) => {
  const msg = reason instanceof Error ? reason.message : String(reason);
  if (msg.includes('Transport not started')) return;
  logger.error('unhandledRejection: %s', msg);
});

main().catch((err) => {
  logger.error('fatal: %s', err instanceof Error ? err.stack ?? err.message : String(err));
  process.exit(1);
});
