// 界面烟雾测试：通过 WebView2 远程调试端口检查 CSP 收紧后前端是否仍可用。
//
// 用法：
//   1) $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=9222"
//   2) 启动 lagm-desktop.exe
//   3) node scripts/verify-ui-csp.mjs [port]
//
// 检查项：
//   - 导航条目渲染（React 是否挂载）
//   - blob: 动态 import 是否可用（UI 插件的加载方式）
//   - 内联 script 是否被 CSP 拦截（负向验证，确认 CSP 真的生效）

const port = process.argv[2] ?? '9222';

const EXPRESSION = `(async () => {
  const result = {};
  result.title = document.title;
  result.navItems = document.querySelectorAll('nav .nav-item').length;
  const meta = document.querySelector('meta[http-equiv="Content-Security-Policy"]');
  result.cspMeta = meta ? meta.getAttribute('content') : null;

  try {
    const url = URL.createObjectURL(new Blob(['export function mount(){return "ok"}'], { type: 'text/javascript' }));
    const mod = await import(url);
    result.blobImport = typeof mod.mount === 'function' ? 'ok' : 'missing-export';
    URL.revokeObjectURL(url);
  } catch (err) {
    result.blobImport = 'error: ' + (err && err.message ? err.message : String(err));
  }

  try {
    const script = document.createElement('script');
    script.textContent = 'window.__inlineRan = true;';
    document.head.appendChild(script);
    script.remove();
    result.inlineScript = window.__inlineRan ? 'ran' : 'blocked';
  } catch (err) {
    result.inlineScript = 'threw: ' + (err && err.message ? err.message : String(err));
  }

  return JSON.stringify(result);
})()`;

async function findTarget() {
  for (let attempt = 0; attempt < 20; attempt += 1) {
    try {
      const response = await fetch(`http://127.0.0.1:${port}/json`);
      const targets = await response.json();
      const page = targets.find((item) => item.type === 'page' && item.webSocketDebuggerUrl);
      if (page) return page;
    } catch {
      // 端口还没起来，等下一轮。
    }
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  return null;
}

function evaluate(wsUrl) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(wsUrl);
    const timer = setTimeout(() => {
      socket.close();
      reject(new Error('CDP 超时'));
    }, 20000);
    socket.addEventListener('open', () => {
      socket.send(
        JSON.stringify({
          id: 1,
          method: 'Runtime.evaluate',
          params: { expression: EXPRESSION, awaitPromise: true, returnByValue: true },
        }),
      );
    });
    socket.addEventListener('message', (event) => {
      const message = JSON.parse(event.data);
      if (message.id !== 1) return;
      clearTimeout(timer);
      socket.close();
      if (message.error) {
        reject(new Error(JSON.stringify(message.error)));
        return;
      }
      resolve(message.result);
    });
    socket.addEventListener('error', () => {
      clearTimeout(timer);
      reject(new Error('CDP 连接失败'));
    });
  });
}

const target = await findTarget();
if (!target) {
  console.error(`没有找到调试目标（端口 ${port}）。应用是否以远程调试参数启动？`);
  process.exit(1);
}

const outcome = await evaluate(target.webSocketDebuggerUrl);
const raw = outcome.result?.value;
if (typeof raw !== 'string') {
  console.error('页面没有返回预期结果：', JSON.stringify(outcome));
  process.exit(1);
}

const data = JSON.parse(raw);
console.log(JSON.stringify(data, null, 2));

const failures = [];
if (!(data.navItems >= 3)) failures.push('导航条目没有渲染（React 未挂载或白屏）');
if (data.blobImport !== 'ok') failures.push(`blob: 动态 import 失败：${data.blobImport}`);
if (data.inlineScript !== 'blocked') failures.push(`内联脚本未被拦截：${data.inlineScript}`);

if (failures.length > 0) {
  console.error('失败项：');
  for (const item of failures) console.error(` - ${item}`);
  process.exit(1);
}
console.log('界面与 CSP 检查通过');
