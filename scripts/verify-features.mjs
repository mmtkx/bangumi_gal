// 功能烟雾测试：通过 WebView2 远程调试检查本轮新增的界面能力是否真的可用。
//
// 用法：
//   WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222 <app>.exe
//   node scripts/verify-features.mjs [port]
//
// 只读检查为主：会勾选/取消列表里的条目，但不会点击「打开目录」「移入回收站」等会改变系统状态的操作。

const port = process.argv[2] ?? '9222';
const waits = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function findTarget() {
  for (let attempt = 0; attempt < 20; attempt += 1) {
    try {
      const response = await fetch(`http://127.0.0.1:${port}/json`);
      const targets = await response.json();
      const page = targets.find((item) => item.type === 'page' && item.webSocketDebuggerUrl);
      if (page) return page;
    } catch {
      // 端口未就绪。
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

async function waitFor(expression, description, timeoutMs = 8000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const value = await evaluate(expression);
    if (value) return value;
    await waits(250);
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

const checks = [];
const record = (name, value) => checks.push({ name, value });
const failures = [];
const expect = (ok, message) => {
  if (!ok) failures.push(message);
};

try {
  // 0. 先回到条目库（脚本可能在设置页收尾，重跑时要先切回来）。
  await evaluate(`(() => {
    const nav = Array.from(document.querySelectorAll('nav .nav-item')).find((el) => el.textContent.includes('条目库'));
    if (nav) nav.click();
    return true;
  })()`);
  // 注意别把「正在读取条目…」当成渲染完成。
  await waitFor(
    `Boolean(document.querySelector('table.table')) || (document.querySelector('.placeholder')?.innerText ?? '').includes('还没有条目')`,
    '条目库渲染',
  );

  // 1. 列表页：排序与绑定状态筛选就位。
  const selects = await evaluate(`(() => {
    const all = Array.from(document.querySelectorAll('.toolbar select'));
    return all.map((el) => Array.from(el.options).map((option) => option.textContent));
  })()`);
  record('列表筛选/排序', JSON.stringify(selects));
  expect(selects.length === 2, '列表页应有「绑定状态」与「排序」两个下拉');
  expect(
    selects.some((options) => options.includes('按最近更新')),
    '排序下拉缺少「按最近更新」',
  );

  // 2. 批量选择：勾选全选后出现批量操作条。
  const rows = await evaluate(`document.querySelectorAll('tbody tr.row').length`);
  record('条目数', rows);
  if (rows > 0) {
    const batch = await evaluate(`(() => {
      const box = document.querySelector('thead .col-check input');
      if (!box) return null;
      box.click();
      return document.querySelector('.batch-bar')?.innerText ?? '';
    })()`);
    record('批量操作条', (batch ?? '').replace(/\s+/g, ' ').trim());
    expect((batch ?? '').includes('已选'), '勾选后应出现「已选 N 个」批量操作条');
    expect((batch ?? '').includes('刷新元数据'), '批量操作条缺少「刷新元数据」');
    expect((batch ?? '').includes('移入回收站'), '批量操作条缺少「移入回收站」');

    // 取消选择，避免影响后续操作。
    await evaluate(`(() => {
      const box = document.querySelector('thead .col-check input');
      if (box && box.checked) box.click();
      return true;
    })()`);
  }

  // 3. 详情页：头部快捷操作与连播开关。
  if (rows > 0) {
    await evaluate(`document.querySelector('tbody tr.row').click(); true`);
    const detailButtons = await waitFor(
      `(() => {
        const text = Array.from(document.querySelectorAll('.page-head button')).map((el) => el.textContent.trim());
        return text.length > 0 ? JSON.stringify(text) : '';
      })()`,
      '详情页头部按钮',
    );
    record('详情头部按钮', detailButtons);
    expect(detailButtons.includes('打开目录'), '详情页缺少「打开目录」');
    expect(detailButtons.includes('复制路径'), '详情页缺少「复制路径」');

    const hasPullCloud = await evaluate(
      `Array.from(document.querySelectorAll('.card button')).some((el) => el.textContent.includes('从 Bangumi 拉取'))`,
    );
    record('拉取云端按钮', hasPullCloud);
    expect(hasPullCloud, '详情页「收藏与评分」缺少「从 Bangumi 拉取」');

    const hasAutoNext = await evaluate(
      `document.querySelectorAll('.checkbox-inline').length > 0 && document.body.innerText.includes('连播')`,
    );
    record('连播开关', hasAutoNext);
    expect(hasAutoNext, '动画详情页缺少「连播」开关');

    // 回到列表。
    await evaluate(`(() => {
      const back = Array.from(document.querySelectorAll('.page-head button')).find((el) => el.textContent.includes('返回'));
      if (back) back.click();
      return true;
    })()`);
  }

  // 4. 设置页：元数据缓存卡片与日志目录按钮。
  await evaluate(`(() => {
    const nav = Array.from(document.querySelectorAll('nav .nav-item')).find((el) => el.textContent.includes('设置'));
    if (nav) nav.click();
    return true;
  })()`);
  const settingsText = await waitFor(
    `(() => { const main = document.querySelector('main'); return main && main.innerText.includes('元数据缓存') ? main.innerText : ''; })()`,
    '设置页缓存卡片',
  );
  expect(settingsText.includes('元数据缓存'), '设置页缺少「元数据缓存」卡片');
  expect(settingsText.includes('清空缓存'), '缓存卡片缺少「清空缓存」');
  expect(settingsText.includes('打开目录'), '日志卡片缺少「打开目录」');
  const cacheLine = settingsText
    .split('\n')
    .find((line) => line.includes('当前占用'));
  record('缓存占用行', (cacheLine ?? '').trim());
  expect(Boolean(cacheLine), '缓存卡片缺少「当前占用」信息');

  // 5. 宽屏布局：模拟 1920 宽，内容应居中且有最大宽度，不能出现横向滚动。
  await send('Emulation.setDeviceMetricsOverride', {
    width: 1920,
    height: 1080,
    deviceScaleFactor: 1,
    mobile: false,
  });
  const layout = await evaluate(`(() => {
    const page = document.querySelector('.page');
    const main = document.querySelector('main');
    if (!page || !main) return null;
    const pr = page.getBoundingClientRect();
    const mr = main.getBoundingClientRect();
    return {
      pageWidth: Math.round(pr.width),
      leftGap: Math.round(pr.left - mr.left),
      // 用 clientWidth 扣掉滚动条，否则右侧会多出一条 16px 的“留白”。
      rightGap: Math.round(mr.left + main.clientWidth - pr.right),
      scrollWidth: document.documentElement.scrollWidth,
      viewWidth: window.innerWidth,
    };
  })()`);
  await send('Emulation.clearDeviceMetricsOverride', {});
  record('1920 宽布局', JSON.stringify(layout));
  expect(Boolean(layout), '拿不到布局尺寸');
  if (layout) {
    expect(layout.pageWidth <= 1444, `内容宽度应受上限约束，实际 ${layout.pageWidth}`);
    expect(
      Math.abs(layout.leftGap - layout.rightGap) <= 2,
      `内容应水平居中，左右留白 ${layout.leftGap} / ${layout.rightGap}`,
    );
    expect(layout.scrollWidth <= layout.viewWidth + 1, '出现横向滚动');
  }

  // 6. 账号页：头像、昵称与授权区块；授权区块不应再出现在设置页。
  await evaluate(`(() => {
    const nav = Array.from(document.querySelectorAll('nav .nav-item')).find((el) => el.textContent.includes('账号'));
    if (nav) nav.click();
    return true;
  })()`);
  const account = await waitFor(
    `(() => {
      const card = document.querySelector('.account-card');
      const main = document.querySelector('main');
      const refresh = Array.from(document.querySelectorAll('.page-head button')).find((btn) => btn.textContent.includes('刷新'));
      // 「读取中…」说明还没拿到账号资料，等下一轮。
      if (!main || (refresh && refresh.textContent.includes('读取中'))) return '';
      const avatar = document.querySelector('.account-avatar');
      return JSON.stringify({
        authorizedCard: Boolean(card),
        avatarLoaded: avatar && avatar.tagName === 'IMG' ? avatar.naturalWidth > 0 : null,
        avatarSrc: avatar && avatar.tagName === 'IMG' ? avatar.getAttribute('src') : null,
        hasAuthSection: main.innerText.includes('Bangumi 授权'),
        text: (card ? card.innerText : main.innerText).replace(/\\s+/g, ' ').trim().slice(0, 120),
      });
    })()`,
    '账号页渲染',
    25000,
  );
  const accountInfo = JSON.parse(account);
  record('账号页', JSON.stringify(accountInfo));
  expect(accountInfo.hasAuthSection, '账号页缺少「Bangumi 授权」区块');
  if (accountInfo.authorizedCard) {
    expect(accountInfo.avatarLoaded !== false, '头像图片加载失败（检查 CSP img-src）');
  }

  // 7. 设置页不应再包含授权表单。
  await evaluate(`(() => {
    const nav = Array.from(document.querySelectorAll('nav .nav-item')).find((el) => el.textContent.includes('设置'));
    if (nav) nav.click();
    return true;
  })()`);
  await waitFor(
    `(() => { const main = document.querySelector('main'); return main && main.innerText.includes('插件') ? main.innerText : ''; })()`,
    '设置页渲染',
  );
  const settingsAuthInputs = await evaluate(
    `document.querySelectorAll('main input[type="password"]').length`,
  );
  record('设置页密码输入框数', settingsAuthInputs);
  expect(settingsAuthInputs === 0, '设置页仍有授权表单（应已移到账号页）');

  // 8. 同步页：推送队列每行应有删除按钮。
  await evaluate(`(() => {
    const nav = Array.from(document.querySelectorAll('nav .nav-item')).find((el) => el.textContent.includes('同步'));
    if (nav) nav.click();
    return true;
  })()`);
  await waitFor(
    `(() => { const main = document.querySelector('main'); return main && main.innerText.includes('推送队列') ? main.innerText : ''; })()`,
    '同步页渲染',
  );
  const queueRows = await evaluate(
    `(() => {
      const section = Array.from(document.querySelectorAll('section.card')).find((el) =>
        (el.querySelector('h2')?.textContent ?? '').includes('推送队列'),
      );
      if (!section) return { rows: 0, withDelete: 0 };
      const rows = Array.from(section.querySelectorAll('tbody tr'));
      const withDelete = rows.filter((row) => Array.from(row.querySelectorAll('button')).some((btn) => btn.textContent.trim() === '删除')).length;
      return { rows: rows.length, withDelete };
    })()`,
  );
  record('同步队列删除按钮', JSON.stringify(queueRows));
  if (queueRows.rows > 0) {
    expect(queueRows.withDelete === queueRows.rows, '推送队列存在没有删除按钮的行');
  }

  console.log(JSON.stringify(checks, null, 2));
  if (failures.length > 0) {
    console.error('失败项：');
    for (const item of failures) console.error(` - ${item}`);
    process.exitCode = 1;
  } else {
    console.log('新功能界面检查通过');
  }
} catch (err) {
  console.error('检查失败：', err.message);
  // 5. 宽屏布局：模拟 1920 宽，内容应居中且有最大宽度，不能出现横向滚动。
  await send('Emulation.setDeviceMetricsOverride', {
    width: 1920,
    height: 1080,
    deviceScaleFactor: 1,
    mobile: false,
  });
  const layout = await evaluate(`(() => {
    const page = document.querySelector('.page');
    const main = document.querySelector('main');
    if (!page || !main) return null;
    const pr = page.getBoundingClientRect();
    const mr = main.getBoundingClientRect();
    return {
      pageWidth: Math.round(pr.width),
      leftGap: Math.round(pr.left - mr.left),
      // 用 clientWidth 扣掉滚动条，否则右侧会多出一条 16px 的“留白”。
      rightGap: Math.round(mr.left + main.clientWidth - pr.right),
      scrollWidth: document.documentElement.scrollWidth,
      viewWidth: window.innerWidth,
    };
  })()`);
  await send('Emulation.clearDeviceMetricsOverride', {});
  record('1920 宽布局', JSON.stringify(layout));
  expect(Boolean(layout), '拿不到布局尺寸');
  if (layout) {
    expect(layout.pageWidth <= 1444, `内容宽度应受上限约束，实际 ${layout.pageWidth}`);
    expect(
      Math.abs(layout.leftGap - layout.rightGap) <= 2,
      `内容应水平居中，左右留白 ${layout.leftGap} / ${layout.rightGap}`,
    );
    expect(layout.scrollWidth <= layout.viewWidth + 1, '出现横向滚动');
  }

  // 6. 账号页：头像、昵称与授权区块；授权区块不应再出现在设置页。
  await evaluate(`(() => {
    const nav = Array.from(document.querySelectorAll('nav .nav-item')).find((el) => el.textContent.includes('账号'));
    if (nav) nav.click();
    return true;
  })()`);
  const account = await waitFor(
    `(() => {
      const card = document.querySelector('.account-card');
      const main = document.querySelector('main');
      const refresh = Array.from(document.querySelectorAll('.page-head button')).find((btn) => btn.textContent.includes('刷新'));
      // 「读取中…」说明还没拿到账号资料，等下一轮。
      if (!main || (refresh && refresh.textContent.includes('读取中'))) return '';
      const avatar = document.querySelector('.account-avatar');
      return JSON.stringify({
        authorizedCard: Boolean(card),
        avatarLoaded: avatar && avatar.tagName === 'IMG' ? avatar.naturalWidth > 0 : null,
        avatarSrc: avatar && avatar.tagName === 'IMG' ? avatar.getAttribute('src') : null,
        hasAuthSection: main.innerText.includes('Bangumi 授权'),
        text: (card ? card.innerText : main.innerText).replace(/\\s+/g, ' ').trim().slice(0, 120),
      });
    })()`,
    '账号页渲染',
    25000,
  );
  const accountInfo = JSON.parse(account);
  record('账号页', JSON.stringify(accountInfo));
  expect(accountInfo.hasAuthSection, '账号页缺少「Bangumi 授权」区块');
  if (accountInfo.authorizedCard) {
    expect(accountInfo.avatarLoaded !== false, '头像图片加载失败（检查 CSP img-src）');
  }

  // 7. 设置页不应再包含授权表单。
  await evaluate(`(() => {
    const nav = Array.from(document.querySelectorAll('nav .nav-item')).find((el) => el.textContent.includes('设置'));
    if (nav) nav.click();
    return true;
  })()`);
  await waitFor(
    `(() => { const main = document.querySelector('main'); return main && main.innerText.includes('插件') ? main.innerText : ''; })()`,
    '设置页渲染',
  );
  const settingsAuthInputs = await evaluate(
    `document.querySelectorAll('main input[type="password"]').length`,
  );
  record('设置页密码输入框数', settingsAuthInputs);
  expect(settingsAuthInputs === 0, '设置页仍有授权表单（应已移到账号页）');

  // 8. 同步页：推送队列每行应有删除按钮。
  await evaluate(`(() => {
    const nav = Array.from(document.querySelectorAll('nav .nav-item')).find((el) => el.textContent.includes('同步'));
    if (nav) nav.click();
    return true;
  })()`);
  await waitFor(
    `(() => { const main = document.querySelector('main'); return main && main.innerText.includes('推送队列') ? main.innerText : ''; })()`,
    '同步页渲染',
  );
  const queueRows = await evaluate(
    `(() => {
      const section = Array.from(document.querySelectorAll('section.card')).find((el) =>
        (el.querySelector('h2')?.textContent ?? '').includes('推送队列'),
      );
      if (!section) return { rows: 0, withDelete: 0 };
      const rows = Array.from(section.querySelectorAll('tbody tr'));
      const withDelete = rows.filter((row) => Array.from(row.querySelectorAll('button')).some((btn) => btn.textContent.trim() === '删除')).length;
      return { rows: rows.length, withDelete };
    })()`,
  );
  record('同步队列删除按钮', JSON.stringify(queueRows));
  if (queueRows.rows > 0) {
    expect(queueRows.withDelete === queueRows.rows, '推送队列存在没有删除按钮的行');
  }

  console.log(JSON.stringify(checks, null, 2));
  process.exitCode = 1;
} finally {
  socket.close();
}
