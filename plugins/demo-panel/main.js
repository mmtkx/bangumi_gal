// 示例 UI 插件：在条目详情页显示一个面板。
// 宿主会注入 { apiVersion, pluginId, listItems(), emit() }，
// 其中 listItems 需要 manifest 声明 read:library，emit 需要 emit:events。

export default {
  mount(container, host, context) {
    const box = document.createElement('div');

    const title = document.createElement('p');
    title.textContent = `示例插件已加载（API ${host.apiVersion}，${host.pluginId}）`;
    box.appendChild(title);

    const info = document.createElement('p');
    info.className = 'muted';
    info.textContent = `当前条目：${context.itemId ?? '未知'}`;
    box.appendChild(info);

    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'btn btn-tiny';
    button.textContent = '读取条目数';
    button.onclick = async () => {
      try {
        const items = await host.listItems();
        info.textContent = `共 ${items.length} 个条目（read:library 权限生效）`;
      } catch (err) {
        info.textContent = `读取失败：${err && err.message ? err.message : err}`;
      }
    };
    box.appendChild(button);

    container.appendChild(box);
    return () => box.remove();
  },
};
