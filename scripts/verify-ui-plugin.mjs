// UI 插件端到端验证：在真实 WebView 里加载示例插件、挂到条目详情页、调用宿主 API。
//
// 前置：把 plugins/demo-panel 复制到应用插件目录，并以远程调试端口启动应用。
// 用法：node scripts/verify-ui-plugin.mjs <animeRoot> [port]
//
// 该脚本会临时写入设置、扫描目录、启用插件；结束时删除「位于该根目录下」的条目、
// 卸载插件，并把动画根目录恢复成运行前的值（退出码非 0 时也会尽力恢复）。

const animeRoot = process.argv[2];
const port = process.argv[3] ?? '9222';

if (!animeRoot) {
  console.error('缺少参数：动画根目录');
  process.exit(1);
}

const PLUGIN_ID = 'com.lagm.demo-panel';
const waits = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function findTarget() {
  for (let attempt = 0; attempt < 20; attempt += 1) {
    try {
      const response = await fetch(`http://127.0.0.1:${port}/json`);
      const targets = await response.json();
      const page = targets.find((item) => item.type === 'page' && item.webSocketDebuggerUrl);
      if (page) return page;
    } catch {
      // 端口未就绪，继续等。
    }
    await waits(500);
  }
  return null;
}

let socket;
let nextId = 1;
const pending = new Map();

function connect(wsUrl) {
  return new Promise((resolve, reject) => {
    socket = new WebSocket(wsUrl);
    socket.addEventListener('open', () => resolve());
    socket.addEventListener('error', () => reject(new Error('CDP 连接失败')));
    socket.addEventListener('message', (event) => {
      const message = JSON.parse(event.data);
      const entry = pending.get(message.id);
      if (!entry) return;
      pending.delete(message.id);
      entry(message);
    });
  });
}

function send(method, params) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    pending.set(id, (message) => {
      if (message.error) reject(new Error(JSON.stringify(message.error)));
      else resolve(message.result);
    });
    socket.send(JSON.stringify({ id, method, params }));
  });
}

async function evaluate(expression) {
  const result = await send('Runtime.evaluate', {
    expression,
    awaitPromise: true,
    returnByValue: true,
  });
  if (result.exceptionDetails) {
    throw new Error(result.exceptionDetails.exception?.description ?? '页面内执行失败');
  }
  return result.result.value;
}

/** 通过前端 IPC 调用 Tauri 命令。 */
function invoke(command, args = {}) {
  return evaluate(
    `window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)}, ${JSON.stringify(args)})`,
  );
}

async function waitFor(expression, description, timeoutMs = 8000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const value = await evaluate(expression);
    if (value) return value;
    await waits(300);
  }
  throw new Error(`等待超时：${description}`);
}

const target = await findTarget();
if (!target) {
  console.error(`没有找到调试目标（端口 ${port}）`);
  process.exit(1);
}
await connect(target.webSocketDebuggerUrl);
await send('Runtime.enable', {});

const steps = [];
const record = (name, detail) => steps.push({ name, detail });
const normalize = (value) => (value ?? '').replace(/\\/g, '/').toLowerCase();

let previousRoot = null;

try {
  // 0. 记住原设置，测试结束后恢复，避免污染开发库。
  const before = await invoke('get_settings');
  previousRoot = before.animeRoot ?? null;
  record('原设置', `animeRoot=${previousRoot ?? '(未设置)'}`);

  // 1. 配置动画根目录并扫描，制造一个可打开的条目。
  await invoke('save_settings', { input: { animeRoot } });
  await invoke('scan_library');

  // 2. 启用示例插件（启动时会被自动登记为未启用）。
  const overview = await invoke('plugin_list');
  const plugin = overview.plugins.find((item) => item.id === PLUGIN_ID);
  if (!plugin) throw new Error(`插件未登记：${PLUGIN_ID}`);
  if (!plugin.enabled) await invoke('plugin_set_enabled', { id: PLUGIN_ID, enabled: true });
  record('插件', `${plugin.id} kind=${plugin.kind} mount=${plugin.mount}`);

  // 3. 只认本次扫描出来的条目：路径必须落在传入的根目录下，避免误删已有条目。
  const items = await invoke('list_items');
  const targetItem = items.find((item) => normalize(item.folderPath).startsWith(normalize(animeRoot)));
  if (!targetItem) throw new Error(`没有找到位于 ${animeRoot} 下的条目`);
  record('目标条目', `${targetItem.title} → ${targetItem.folderPath}`);

  await waitFor(`document.querySelectorAll('tbody tr.row').length > 0`, '条目列表渲染');
  const clicked = await evaluate(`(() => {
    const rows = Array.from(document.querySelectorAll('tbody tr.row'));
    const row = rows.find((candidate) => candidate.innerText.includes(${JSON.stringify(targetItem.title)}));
    if (!row) return false;
    row.click();
    return true;
  })()`);
  if (!clicked) throw new Error('条目列表里找不到目标行');

  const panelText = await waitFor(
    `(() => { const el = document.querySelector('.plugin-panel'); return el && el.innerText ? el.innerText : ''; })()`,
    '插件面板挂载',
  );
  if (!panelText.includes('示例插件已加载')) {
    throw new Error(`面板内容不符合预期：${panelText}`);
  }
  if (!panelText.includes(targetItem.id)) {
    throw new Error(`面板拿到的条目 id 与目标不一致：${panelText}`);
  }
  record('挂载', panelText.replace(/\s+/g, ' ').trim());

  // 4. 点击插件里的按钮，验证 read:library 权限真的打通。
  await evaluate(`(() => {
    const buttons = Array.from(document.querySelectorAll('.plugin-panel button'));
    const target = buttons.find((btn) => btn.textContent.includes('读取条目数'));
    if (!target) return false;
    target.click();
    return true;
  })()`);
  const permissionText = await waitFor(
    `(() => { const el = document.querySelector('.plugin-panel'); return el && el.innerText.includes('共 ') ? el.innerText : ''; })()`,
    'read:library 调用返回',
  );
  record('宿主 API', permissionText.replace(/\s+/g, ' ').trim());

  // 5. 清理：删除本次扫描出的条目、卸载插件、恢复原设置。
  await invoke('delete_item', {
    id: targetItem.id,
    mode: 'permanent',
    confirm: targetItem.title,
  });
  await invoke('plugin_uninstall', { id: PLUGIN_ID });
  await invoke('save_settings', { input: { animeRoot: previousRoot } });
  const restored = await invoke('get_settings');
  if ((restored.animeRoot ?? null) !== previousRoot) {
    throw new Error(`根目录未恢复：${restored.animeRoot}`);
  }
  record('清理', `已删除条目 ${targetItem.id}、卸载插件、恢复 animeRoot=${previousRoot ?? '(未设置)'}`);

  console.log(JSON.stringify(steps, null, 2));
  console.log('UI 插件端到端验证通过');
} catch (err) {
  console.error('验证失败：', err.message);
  console.log(JSON.stringify(steps, null, 2));
  // 失败也要尽力恢复设置，避免留下错误的根目录。
  if (previousRoot !== undefined) {
    try {
      await invoke('save_settings', { input: { animeRoot: previousRoot } });
      console.error(`已恢复 animeRoot=${previousRoot ?? '(未设置)'}`);
    } catch (restoreErr) {
      console.error('恢复设置失败：', restoreErr.message);
    }
  }
  process.exitCode = 1;
} finally {
  socket.close();
}
