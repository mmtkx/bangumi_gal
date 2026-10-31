// UI 插件加载器：把已启用的 UI 插件以 ESM 方式动态导入并挂载到指定挂载点。
//
// 说明：
// - 入口源码由后端读取并通过 Blob URL 导入（CSP 为 null，允许 blob: 动态 import）。
// - 宿主 API 的每个方法都会回到后端做权限校验，前端不做信任判断。
// - 单个插件加载/挂载失败只记录日志，不影响其他插件与主界面。

import {
  pluginEmit,
  pluginList,
  pluginReadItems,
  pluginUiEntry,
  pluginUiUnloaded,
  type ItemSummary,
  type PluginInfo,
} from '../api';

export interface PluginHostApi {
  apiVersion: '1';
  pluginId: string;
  /** 读取条目列表；需要 read:library 权限。 */
  listItems: () => Promise<ItemSummary[]>;
  /** 发送插件事件；需要 emit:events 权限。 */
  emit: (name: string, payload?: unknown) => Promise<void>;
}

export interface PluginModule {
  mount?: (
    container: HTMLElement,
    host: PluginHostApi,
    context: Record<string, unknown>,
  ) => void | (() => void);
  unmount?: () => void;
}

export interface MountedPlugin {
  info: PluginInfo;
  dispose: () => void;
}

export async function mountUiPlugins(
  mountPoint: string,
  container: HTMLElement,
  context: Record<string, unknown>,
): Promise<MountedPlugin[]> {
  let plugins: PluginInfo[];
  try {
    const overview = await pluginList();
    plugins = overview.plugins;
  } catch (err) {
    console.warn('读取插件列表失败', err);
    return [];
  }

  const candidates = plugins.filter(
    (plugin) =>
      plugin.installed &&
      plugin.enabled &&
      plugin.kind === 'ui' &&
      plugin.error === null &&
      plugin.mount === mountPoint,
  );

  const mounted: MountedPlugin[] = [];
  for (const info of candidates) {
    try {
      const entry = await pluginUiEntry(info.id);
      const blob = new Blob([entry.source], { type: 'text/javascript' });
      const url = URL.createObjectURL(blob);
      let module: Record<string, unknown>;
      try {
        module = (await import(/* @vite-ignore */ url)) as Record<string, unknown>;
      } finally {
        URL.revokeObjectURL(url);
      }
      const plugin = ((module.default as PluginModule | undefined) ?? module) as PluginModule;

      const host: PluginHostApi = {
        apiVersion: '1',
        pluginId: info.id,
        listItems: () => pluginReadItems(info.id),
        emit: (name, payload) => pluginEmit(info.id, name, payload),
      };

      const element = document.createElement('div');
      element.className = 'plugin-panel';
      container.appendChild(element);

      let cleanup: void | (() => void);
      try {
        cleanup = plugin.mount?.(element, host, context);
      } catch (err) {
        console.error(`插件 ${info.id} 挂载失败`, err);
        element.textContent = '插件运行出错，已隔离。';
        element.classList.add('plugin-panel-error');
      }

      mounted.push({
        info,
        dispose: () => {
          try {
            if (typeof cleanup === 'function') cleanup();
            else plugin.unmount?.();
          } catch (err) {
            console.warn(`插件 ${info.id} 卸载出错`, err);
          }
          element.remove();
          void pluginUiUnloaded(info.id).catch(() => undefined);
        },
      });
    } catch (err) {
      console.warn(`加载插件 ${info.id} 失败`, err);
    }
  }

  return mounted;
}
