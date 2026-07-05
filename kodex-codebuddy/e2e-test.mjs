// End-to-end tests against the running proxy at http://127.0.0.1:17856
const BASE = process.env.PROXY_URL ?? 'http://127.0.0.1:17856';

async function post(path, body) {
  const res = await fetch(BASE + path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(`${path} ${res.status}: ${await res.text()}`);
  return res;
}

async function testModels() {
  const res = await fetch(BASE + '/v1/models');
  const json = await res.json();
  console.assert(json.object === 'list', 'models.object === list');
  console.assert(json.data.length > 0, 'models.data non-empty');
  console.log(`  /v1/models -> ${json.data.length} models`);
}

async function testNonStreaming() {
  const res = await post('/v1/chat/completions', {
    model: 'claude-sonnet-5',
    messages: [{ role: 'user', content: 'Reply with exactly the word: PONG' }],
    stream: false,
  });
  const json = await res.json();
  console.assert(json.object === 'chat.completion', 'object');
  console.assert(json.choices.length === 1, 'one choice');
  const text = json.choices[0].message.content ?? '';
  console.assert(text.includes('PONG'), `content contains PONG: ${text}`);
  console.assert(json.usage.total_tokens >= 0, 'usage present');
  console.log(`  /v1/chat/completions (non-stream) -> "${text.slice(0, 40)}" usage=${JSON.stringify(json.usage)}`);
}

async function testStreaming() {
  const res = await post('/v1/chat/completions', {
    model: 'claude-sonnet-5',
    messages: [{ role: 'user', content: 'Count from 1 to 5, one number per line, nothing else.' }],
    stream: true,
  });
  let assembled = '';
  let sawRole = false;
  let sawDone = false;
  let finishReason = null;
  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buf = '';
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    buf += decoder.decode(value, { stream: true });
    let idx;
    while ((idx = buf.indexOf('\n\n')) !== -1) {
      const frame = buf.slice(0, idx);
      buf = buf.slice(idx + 2);
      if (!frame.startsWith('data: ')) continue;
      const payload = frame.slice(6);
      if (payload === '[DONE]') { sawDone = true; continue; }
      const obj = JSON.parse(payload);
      if (obj.choices?.[0]?.delta?.role) sawRole = true;
      if (obj.choices?.[0]?.delta?.content) assembled += obj.choices[0].delta.content;
      if (obj.choices?.[0]?.finish_reason) finishReason = obj.choices[0].finish_reason;
    }
  }
  console.assert(sawRole, 'stream: saw role delta');
  console.assert(sawDone, 'stream: saw [DONE]');
  console.assert(finishReason === 'stop', `stream: finish_reason=stop (got ${finishReason})`);
  console.log(`  /v1/chat/completions (stream) -> "${assembled.replace(/\n/g, ' | ')}"`);
}

async function testToolCallPassthrough() {
  const res = await post('/v1/chat/completions', {
    model: 'claude-sonnet-5',
    messages: [{ role: 'user', content: "What's the weather in Tokyo? Use the get_weather tool." }],
    tools: [
      {
        type: 'function',
        function: {
          name: 'get_weather',
          description: 'Get current weather for a location',
          parameters: {
            type: 'object',
            properties: { location: { type: 'string', description: 'City name' } },
            required: ['location'],
          },
        },
      },
    ],
    stream: false,
  });
  const json = await res.json();
  const choice = json.choices[0];
  console.assert(choice.finish_reason === 'tool_calls', `finish_reason=tool_calls (got ${choice.finish_reason})`);
  console.assert(choice.message.tool_calls?.length === 1, 'one tool_call');
  const tc = choice.message.tool_calls[0];
  console.assert(tc.function.name === 'get_weather', `tool name=get_weather (got ${tc.function.name})`);
  const args = JSON.parse(tc.function.arguments);
  console.assert(args.location && args.location.toLowerCase().includes('tokyo'), `args.location ~ tokyo (got ${JSON.stringify(args)})`);
  console.log(`  tool_call passthrough -> name=${tc.function.name} args=${tc.function.arguments} id=${tc.id}`);
}

/** Multi-turn reuse via X-Session-Id. */
async function testSessionReuse() {
  const sid = 'e2e-reuse-' + Date.now();
  const headers = { 'Content-Type': 'application/json', 'X-Session-Id': sid };

  // Turn 1: set a "secret" the model must remember.
  const r1 = await fetch(BASE + '/v1/chat/completions', {
    method: 'POST',
    headers,
    body: JSON.stringify({
      model: 'claude-sonnet-5',
      messages: [{ role: 'user', content: 'Remember the secret word: BANANA. Just reply "OK".' }],
      stream: false,
    }),
  });
  const j1 = await r1.json();
  console.assert(r1.headers.get('x-session-id') === sid, 'echoed X-Session-Id');
  console.assert((j1.choices[0].message.content ?? '').includes('OK'), 'turn1 ack');

  // Turn 2: ask the model to recall the secret WITHOUT re-stating it.
  const r2 = await fetch(BASE + '/v1/chat/completions', {
    method: 'POST',
    headers,
    body: JSON.stringify({
      model: 'claude-sonnet-5',
      messages: [{ role: 'user', content: 'What was the secret word I told you? Reply with only the word.' }],
      stream: false,
    }),
  });
  const j2 = await r2.json();
  const ans = (j2.choices[0].message.content ?? '').toUpperCase();
  console.assert(ans.includes('BANANA'), `turn2 recalled BANANA (got "${ans}")`);
  console.log(`  session reuse ${sid} -> turn2 recall: "${ans}"`);

  // Explicit release.
  const del = await fetch(BASE + '/v1/sessions/' + sid, { method: 'DELETE' });
  console.assert(del.ok, 'DELETE session ok');
}

const tests = [
  ['models', testModels],
  ['non-streaming', testNonStreaming],
  ['streaming', testStreaming],
  ['tool-call passthrough', testToolCallPassthrough],
  ['session reuse (X-Session-Id)', testSessionReuse],
];

let failures = 0;
for (const [name, fn] of tests) {
  try {
    await fn();
    console.log(`PASS: ${name}`);
  } catch (err) {
    failures++;
    console.error(`FAIL: ${name} -> ${err instanceof Error ? err.stack ?? err.message : String(err)}`);
  }
}
console.log(failures === 0 ? `\nAll ${tests.length} tests passed.` : `\n${failures}/${tests.length} tests FAILED.`);
process.exit(failures === 0 ? 0 : 1);
