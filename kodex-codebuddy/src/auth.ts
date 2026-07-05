/** API-key authentication for the proxy. */
import type { Request, Response, NextFunction } from 'express';
import { logger } from './logger.js';

export interface AuthConfig {
  /** Expected key; empty string disables auth. */
  apiKey: string;
}

export function createAuthMiddleware(cfg: AuthConfig) {
  return function authMiddleware(req: Request, res: Response, next: NextFunction): void {
    if (!cfg.apiKey) {
      next();
      return;
    }
    const bearer = req.headers['authorization'];
    const xKey = req.headers['x-api-key'];
    const provided =
      (typeof bearer === 'string' ? bearer.replace(/^Bearer\s+/i, '') : '') ||
      (typeof xKey === 'string' ? xKey : '');
    if (provided !== cfg.apiKey) {
      logger.warn('auth rejected: missing/invalid api key (%s %s)', req.method, req.path);
      res.status(401).json({ error: { message: 'Invalid API key', type: 'invalid_request_error' } });
      return;
    }
    next();
  };
}
