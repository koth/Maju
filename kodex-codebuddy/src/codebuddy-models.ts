/**
 * Model enumeration: use the v2 session API's `getAvailableModels()` to list
 * CodeBuddy models and map them to the OpenAI `GET /v1/models` shape.
 */
import { unstable_v2_createSession } from '@tencent-ai/agent-sdk';
import type { OAIModelsResponse, OAIModel } from './openai-types.js';
import { logger } from './logger.js';

let cachedModels: OAIModelsResponse | null = null;

export async function listModels(): Promise<OAIModelsResponse> {
  if (cachedModels) return cachedModels;
  const session = unstable_v2_createSession({});
  try {
    await session.connect();
    const models = await session.getAvailableModels();
    const now = Math.floor(Date.now() / 1000);
    const data: OAIModel[] = models.map((m) => ({
      id: m.modelId,
      object: 'model',
      created: now,
      owned_by: 'codebuddy',
    }));
    cachedModels = { object: 'list', data };
    logger.info('enumerated %d codebuddy models', data.length);
    return cachedModels;
  } finally {
    session.close();
  }
}

/** Force a re-enumeration (e.g. after a model config change). */
export function resetModelCache(): void {
  cachedModels = null;
}
