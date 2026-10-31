import { useEffect, useState } from 'react';

import {
  clearMetadataCache,
  formatBytes,
  getSettings,
  logLocation,
  metadataCacheStats,
  openLogDir,
  pluginInstall,
  pluginInvoke,
  pluginList,
  pluginSetEnabled,
  pluginUninstall,
  saveSettings,
  type CacheStats,
  type LogLocation,
  type PluginInfo,
  type PluginInvocation,
  type PluginOverview,
  type Settings,
} from '../api';
import DirectoryPicker from '../components/DirectoryPicker';
import ExePicker from '../components/ExePicker';

type PickerTarget = 'anime' | 'game' | 'cache' | 'player' | 'plugin' | 'plugins';

interface Props {
  onSaved: () => void;
}

const PICKER_TITLE: Record<PickerTarget, string> = {
  anime: '选择动画根目录',
  game: '选择游戏根目录',
  cache: '选择元数据缓存目录',
  player: '选择外部播放器',
  plugin: '选择插件目录（含 manifest.json）',
  plugins: '选择插件根目录',
};

/** 常见播放器的断点续播参数；{file} 与 {seconds} 由后端替换。 */
const ARGS_PRESETS: { label: string; value: string }[] = [
  // PotPlayer 的 /new 必须放在文件前面，否则 /seek 会被忽略（实测）。
  { label: 'PotPlayer', value: '/new "{file}" /seek={seconds}' },
  { label: 'mpv', value: '--start={seconds} "{file}"' },
  { label: 'VLC', value: '--start-time={seconds} "{file}"' },
  { label: '从头播放', value: '"{file}"' },
];

export default function SettingsPage({ onSaved }: Props) {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [animeRoot, setAnimeRoot] = useState('');
  const [gameRoot, setGameRoot] = useState('');
  const [cacheDir, setCacheDir] = useState('');
  const [pluginsDir, setPluginsDir] = useState('');
  const [playerPath, setPlayerPath] = useState('');
  const [playerArgs, setPlayerArgs] = useState('');
  const [picker, setPicker] = useState<PickerTarget | null>(null);
  const [plugins, setPlugins] = useState<PluginOverview | null>(null);
  const [pluginBusy, setPluginBusy] = useState<string | null>(null);
  const [logs, setLogs] = useState<LogLocation | null>(null);
  const [cache, setCache] = useState<CacheStats | null>(null);
  const [cacheBusy, setCacheBusy] = useState(false);
  const [invokingId, setInvokingId] = useState<string | null>(null);
  const [invokeInput, setInvokeInput] = useState('{"file":"[Group] 夏日重现 - 12 [1080p].mkv"}');
  const [invokeResult, setInvokeResult] = useState<PluginInvocation | null>(null);
  const [invokeError, setInvokeError] = useState<string | null>(null);
  const [invoking, setInvoking] = useState(false);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    getSettings()
      .then((data) => {
        if (cancelled) return;
        setSettings(data);
        setAnimeRoot(data.animeRoot ?? '');
        setGameRoot(data.gameRoot ?? '');
        setCacheDir(data.metadataCacheDirIsDefault ? '' : data.metadataCacheDir);
        setPluginsDir(data.pluginsDir ?? '');
        setPlayerPath(data.playerPath ?? '');
        setPlayerArgs(data.playerArgs);
        setError(null);
      })
      .catch((err: unknown) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    metadataCacheStats()
      .then((data) => {
        if (!cancelled) setCache(data);
      })
      .catch((err: unknown) => {
        if (!cancelled) console.warn('读取缓存统计失败', err);
      });
    logLocation()
      .then((data) => {
        if (!cancelled) setLogs(data);
      })
      .catch((err: unknown) => {
        if (!cancelled) console.warn('读取日志目录失败', err);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    pluginList()
      .then((data) => {
        if (!cancelled) setPlugins(data);
      })
      .catch((err: unknown) => {
        if (!cancelled) console.warn('读取插件列表失败', err);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function handleSave() {
    setSaving(true);
    setError(null);
    setNotice(null);
    try {
      const saved = await saveSettings({
        animeRoot: animeRoot.trim() === '' ? null : animeRoot.trim(),
        gameRoot: gameRoot.trim() === '' ? null : gameRoot.trim(),
        metadataCacheDir: cacheDir.trim() === '' ? null : cacheDir.trim(),
        playerPath: playerPath.trim() === '' ? null : playerPath.trim(),
        playerArgs: playerArgs.trim() === '' ? null : playerArgs.trim(),
        pluginsDir: pluginsDir.trim() === '' ? null : pluginsDir.trim(),
      });
      setSettings(saved);
      setAnimeRoot(saved.animeRoot ?? '');
      setGameRoot(saved.gameRoot ?? '');
      setCacheDir(saved.metadataCacheDirIsDefault ? '' : saved.metadataCacheDir);
      setPluginsDir(saved.pluginsDir ?? '');
      setPlayerPath(saved.playerPath ?? '');
      setPlayerArgs(saved.playerArgs);
      // 根目录变化会触发后台「扫描 + 自动匹配」，提示里说清楚，免得用户以为要手动点扫描。
      const rootsChanged =
        (settings?.animeRoot ?? null) !== saved.animeRoot ||
        (settings?.gameRoot ?? null) !== saved.gameRoot;
      setNotice(
        rootsChanged
          ? '设置已保存。正在后台扫描新增目录并自动匹配，稍后回条目库查看。'
          : '设置已保存。',
      );
      void refreshPlugins();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  }

  async function refreshCache() {
    try {
      setCache(await metadataCacheStats());
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  async function handleClearCache() {
    if (!window.confirm('清空元数据缓存？条目资料需要重新拉取，条目本身与进度不受影响。')) return;
    setCacheBusy(true);
    setError(null);
    setNotice(null);
    try {
      const result = await clearMetadataCache();
      setNotice(`已清理 ${result.removedDirs} 个缓存目录，释放 ${formatBytes(result.freedBytes)}`);
      await refreshCache();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setCacheBusy(false);
    }
  }

  async function handleOpenLogDir() {
    try {
      await openLogDir();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  async function refreshLogs() {
    try {
      setLogs(await logLocation());
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  async function refreshPlugins() {
    try {
      setPlugins(await pluginList());
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  async function handleInstallPlugin(path: string) {
    setPluginBusy('install');
    setError(null);
    setNotice(null);
    try {
      setPlugins(await pluginInstall(path));
      setNotice('插件已登记（默认禁用），可在列表里启用。');
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setPluginBusy(null);
    }
  }

  async function handleTogglePlugin(id: string, enabled: boolean) {
    setPluginBusy(id);
    setError(null);
    try {
      setPlugins(await pluginSetEnabled(id, enabled));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setPluginBusy(null);
    }
  }

  async function handleUninstallPlugin(id: string) {
    setPluginBusy(id);
    setError(null);
    setNotice(null);
    try {
      setPlugins(await pluginUninstall(id));
      setNotice('已从注册表移除（插件文件保留）。');
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setPluginBusy(null);
    }
  }

  function openInvoke(plugin: PluginInfo) {
    setInvokingId((current) => (current === plugin.id ? null : plugin.id));
    setInvokeResult(null);
    setInvokeError(null);
  }

  async function handleInvoke(id: string) {
    setInvoking(true);
    setInvokeError(null);
    setInvokeResult(null);
    try {
      const text = invokeInput.trim();
      let input: unknown = null;
      if (text !== '') {
        try {
          input = JSON.parse(text);
        } catch {
          throw new Error('输入不是合法 JSON');
        }
      }
      setInvokeResult(await pluginInvoke(id, input));
    } catch (err) {
      setInvokeError(err instanceof Error ? err.message : String(err));
    } finally {
      setInvoking(false);
    }
  }

  function handlePick(target: PickerTarget, path: string) {
    if (target === 'anime') setAnimeRoot(path);
    else if (target === 'game') setGameRoot(path);
    else if (target === 'player') setPlayerPath(path);
    else if (target === 'cache') setCacheDir(path);
    else if (target === 'plugins') setPluginsDir(path);
    else void handleInstallPlugin(path);
    setPicker(null);
  }

  if (loading) {
    return (
      <div className="page">
        <div className="placeholder">正在读取设置…</div>
      </div>
    );
  }

  return (
    <div className="page">
      <header className="page-head">
        <div>
          <h1>设置</h1>
          <p className="muted">配置两个资源根目录与元数据缓存位置，配置项保存在本地 SQLite。</p>
        </div>
        <div className="page-actions">
          <button type="button" className="btn" onClick={onSaved}>
            返回条目库
          </button>
          <button type="button" className="btn btn-primary" onClick={handleSave} disabled={saving}>
            {saving ? '保存中…' : '保存设置'}
          </button>
        </div>
      </header>

      {error && <div className="banner banner-error">{error}</div>}
      {notice && <div className="banner banner-ok">{notice}</div>}

      <section className="card">
        <h2>资源根目录</h2>
        <p className="muted">
          动画根目录与游戏根目录不能相同，也不能互为父子目录（保存时会校验）。
        </p>

        <label className="field">
          <span className="field-label">动画根目录</span>
          <div className="inline-form">
            <input
              className="input"
              placeholder="例如 D:\\Anime"
              value={animeRoot}
              onChange={(event) => setAnimeRoot(event.target.value)}
            />
            <button type="button" className="btn" onClick={() => setPicker('anime')}>
              浏览…
            </button>
          </div>
        </label>

        <label className="field">
          <span className="field-label">游戏根目录</span>
          <div className="inline-form">
            <input
              className="input"
              placeholder="例如 D:\\Games"
              value={gameRoot}
              onChange={(event) => setGameRoot(event.target.value)}
            />
            <button type="button" className="btn" onClick={() => setPicker('game')}>
              浏览…
            </button>
          </div>
        </label>
      </section>

      <section className="card">
        <h2>元数据缓存目录</h2>
        <p className="muted">
          留空使用默认位置：
          <span className="mono">{settings?.defaultMetadataCacheDir}</span>
        </p>
        <label className="field">
          <span className="field-label">缓存目录</span>
          <div className="inline-form">
            <input
              className="input"
              placeholder={settings?.defaultMetadataCacheDir ?? ''}
              value={cacheDir}
              onChange={(event) => setCacheDir(event.target.value)}
            />
            <button type="button" className="btn" onClick={() => setPicker('cache')}>
              浏览…
            </button>
            <button type="button" className="btn" onClick={() => setCacheDir('')}>
              恢复默认
            </button>
          </div>
        </label>
        <p className="muted">
          当前生效：
          <span className="mono">{settings?.metadataCacheDir ?? ''}</span>
          {settings?.metadataCacheDirIsDefault ? '（默认）' : '（自定义）'}
        </p>
      </section>

      <section className="card">
        <h2>播放器</h2>
        <p className="muted">
          留空即使用系统默认播放器：按 .mkv 等扩展名查注册表里的默认打开方式，直接启动关联程序。
          若该程序是 PotPlayer / VLC / mpv / MPC-HC(BE)，会自动带上断点续播参数；
          其他程序无法跳转，播放会从头开始（界面上会提示）。
        </p>
        <label className="field">
          <span className="field-label">播放器路径</span>
          <div className="inline-form">
            <input
              className="input"
              placeholder="例如 C:\\Program Files\\mpv\\mpv.exe"
              value={playerPath}
              onChange={(event) => setPlayerPath(event.target.value)}
            />
            <button type="button" className="btn" onClick={() => setPicker('player')}>
              浏览…
            </button>
            <button type="button" className="btn" onClick={() => setPlayerPath('')}>
              清除
            </button>
          </div>
        </label>
        <p className="muted">
          {settings?.playerPath
            ? `当前：${settings.playerPath}`
            : '当前：未配置（播放会提示先去设置）'}
        </p>

        <label className="field">
          <span className="field-label">启动参数模板（断点续播）</span>
          <input
            className="input"
            placeholder='例如 "{file}" /seek={seconds} /new'
            value={playerArgs}
            onChange={(event) => setPlayerArgs(event.target.value)}
          />
        </label>
        <div className="chip-row">
          {ARGS_PRESETS.map((preset) => (
            <button
              key={preset.label}
              type="button"
              className={playerArgs === preset.value ? 'chip chip-active' : 'chip'}
              onClick={() => setPlayerArgs(preset.value)}
            >
              {preset.label}
            </button>
          ))}
        </div>
        <p className="muted">
          <span className="mono">{'{file}'}</span> 换成视频路径，
          <span className="mono">{'{seconds}'}</span> 换成已看秒数。模板里没有{' '}
          <span className="mono">{'{seconds}'}</span> 时播放器会从头开始。PotPlayer 必须写成{' '}
          <span className="mono">
            /new &quot;{'{file}'}&quot; /seek={'{seconds}'}
          </span>
          ：<span className="mono">/new</span> 放在文件后面会让 <span className="mono">/seek</span>{' '}
          失效。
        </p>
        <p className="muted">
          进度同步：PotPlayer
          可直接读到真实播放位置（拖动进度条也会同步）；其他播放器只能按进程时长估算。
        </p>
      </section>

      <section className="card">
        <div className="card-head">
          <h2>插件（{plugins?.plugins.length ?? 0}）</h2>
          <div className="row-actions">
            <button type="button" className="btn btn-tiny" onClick={() => void refreshPlugins()}>
              刷新
            </button>
            <button
              type="button"
              className="btn btn-tiny"
              onClick={() => setPicker('plugin')}
              disabled={pluginBusy !== null}
            >
              安装插件…
            </button>
          </div>
        </div>
        <label className="field">
          <span className="field-label">插件根目录（留空使用默认）</span>
          <div className="inline-form">
            <input
              className="input"
              placeholder={settings?.defaultPluginsDir ?? ''}
              value={pluginsDir}
              onChange={(event) => setPluginsDir(event.target.value)}
            />
            <button type="button" className="btn" onClick={() => setPicker('plugins')}>
              浏览…
            </button>
            <button type="button" className="btn" onClick={() => setPluginsDir('')}>
              恢复默认
            </button>
          </div>
        </label>
        <p className="muted">修改插件根目录后需点右上角「保存设置」生效。</p>
        <p className="muted">
          当前生效：<span className="mono">{plugins?.root ?? ''}</span>
        </p>
        <p className="muted">
          把插件文件夹放进去并重启，或点「安装插件…」选择目录。UI
          插件挂到详情页/设置页；能力插件（WASM）
          启用后可点「运行…」在沙箱里执行，超出指令预算或越权访问会直接被拒绝。
        </p>
        {!plugins || plugins.plugins.length === 0 ? (
          <div className="placeholder small">还没有插件。</div>
        ) : (
          <div className="plugin-list">
            {plugins.plugins.map((plugin) => (
              <div key={plugin.id} className="plugin-row">
                <div className="plugin-main">
                  <div className="plugin-title">
                    <strong>{plugin.name}</strong>
                    <span className="muted"> v{plugin.version}</span>
                    <span className={`badge badge-${plugin.kind === 'ui' ? 'anime' : 'game'}`}>
                      {plugin.kind === 'ui' ? 'UI' : '能力'}
                    </span>
                    <span className={plugin.enabled ? 'badge badge-bound' : 'badge'}>
                      {plugin.enabled ? '已启用' : '未启用'}
                    </span>
                    {!plugin.installed && <span className="badge">未安装</span>}
                  </div>
                  <div className="muted plugin-meta">
                    <span className="mono">{plugin.id}</span>
                    {' · '}API {plugin.apiVersion || '?'}
                    {plugin.permissions.length > 0 && ` · 权限：${plugin.permissions.join('、')}`}
                    {plugin.capabilities.length > 0 && ` · 能力：${plugin.capabilities.join('、')}`}
                    {plugin.mount && ` · 挂载点：${plugin.mount}`}
                  </div>
                  {plugin.error && <div className="warn-text">{plugin.error}</div>}
                  {invokingId === plugin.id && (
                    <div className="plugin-runner">
                      <textarea
                        className="plugin-runner-input"
                        rows={3}
                        value={invokeInput}
                        onChange={(event) => setInvokeInput(event.target.value)}
                        spellCheck={false}
                      />
                      <div className="row-actions">
                        <button
                          type="button"
                          className="btn btn-tiny"
                          onClick={() => void handleInvoke(plugin.id)}
                          disabled={invoking}
                        >
                          {invoking ? '执行中…' : '执行'}
                        </button>
                        <button
                          type="button"
                          className="btn btn-tiny"
                          onClick={() => {
                            setInvokingId(null);
                            setInvokeResult(null);
                            setInvokeError(null);
                          }}
                        >
                          收起
                        </button>
                      </div>
                      {invokeError && <div className="warn-text">{invokeError}</div>}
                      {invokeResult && (
                        <div className="plugin-runner-result">
                          <div className="muted">
                            耗时 {invokeResult.durationMs} ms · 指令预算 {invokeResult.fuelUsed}
                          </div>
                          <pre className="plugin-runner-output">
                            {JSON.stringify(invokeResult.output, null, 2)}
                          </pre>
                          {invokeResult.logs.length > 0 && (
                            <pre className="plugin-runner-log">{invokeResult.logs.join('\n')}</pre>
                          )}
                        </div>
                      )}
                    </div>
                  )}
                </div>
                <div className="row-actions">
                  {plugin.installed ? (
                    <>
                      <button
                        type="button"
                        className="btn btn-tiny"
                        onClick={() => void handleTogglePlugin(plugin.id, !plugin.enabled)}
                        disabled={pluginBusy !== null || plugin.error !== null}
                      >
                        {plugin.enabled ? '禁用' : '启用'}
                      </button>
                      {plugin.kind === 'capability' && plugin.enabled && (
                        <button
                          type="button"
                          className="btn btn-tiny"
                          onClick={() => openInvoke(plugin)}
                          disabled={pluginBusy !== null}
                        >
                          运行…
                        </button>
                      )}
                      <button
                        type="button"
                        className="btn btn-tiny"
                        onClick={() => void handleUninstallPlugin(plugin.id)}
                        disabled={pluginBusy !== null}
                      >
                        卸载
                      </button>
                    </>
                  ) : (
                    <button
                      type="button"
                      className="btn btn-tiny"
                      onClick={() => void handleInstallPlugin(plugin.dir)}
                      disabled={pluginBusy !== null || plugin.error !== null}
                    >
                      安装
                    </button>
                  )}
                </div>
              </div>
            ))}
          </div>
        )}
      </section>

      <section className="card">
        <div className="card-head">
          <h2>元数据缓存</h2>
          <div className="row-actions">
            <button type="button" className="btn btn-tiny" onClick={() => void refreshCache()}>
              刷新
            </button>
            <button
              type="button"
              className="btn btn-tiny btn-danger"
              onClick={() => void handleClearCache()}
              disabled={cacheBusy}
            >
              {cacheBusy ? '清理中…' : '清空缓存'}
            </button>
          </div>
        </div>
        <p className="muted">
          每个 Bangumi
          条目的资料（标题、简介、封面地址）缓存一个目录；清空只删缓存，不影响条目与进度。
        </p>
        <p className="muted">
          目录：<span className="mono">{cache?.dir ?? '读取中…'}</span>
        </p>
        <p className="muted">
          当前占用：{cache ? formatBytes(cache.totalBytes) : '—'} · {cache?.subjectDirs ?? 0} 个条目
          · {cache?.fileCount ?? 0} 个文件
        </p>
      </section>

      <section className="card">
        <div className="card-head">
          <h2>日志</h2>
          <div className="row-actions">
            <button type="button" className="btn btn-tiny" onClick={() => void handleOpenLogDir()}>
              打开目录
            </button>
            <button type="button" className="btn btn-tiny" onClick={() => void refreshLogs()}>
              刷新
            </button>
          </div>
        </div>
        <p className="muted">
          日志同时输出到控制台与文件，按大小与日期滚动：单文件上限{' '}
          {formatBytes(logs?.maxBytes ?? 0)}，保留最近 {logs?.keepFiles ?? 0} 个归档。
        </p>
        <p className="muted">
          目录：<span className="mono">{logs?.dir ?? '读取中…'}</span>
        </p>
        {logs && logs.files.length > 0 ? (
          <ul className="log-list">
            {logs.files.map((file) => (
              <li key={file.name} className="log-row">
                <span className="mono">{file.name}</span>
                <span className="muted">
                  {formatBytes(file.sizeBytes)} · {file.modifiedAt}
                </span>
              </li>
            ))}
          </ul>
        ) : (
          <div className="placeholder small">还没有日志文件。</div>
        )}
      </section>

      {picker === 'player' ? (
        <ExePicker
          title={PICKER_TITLE.player}
          initialPath={playerPath}
          onPick={(path) => handlePick('player', path)}
          onClose={() => setPicker(null)}
        />
      ) : (
        picker && (
          <DirectoryPicker
            title={PICKER_TITLE[picker]}
            initialPath={
              picker === 'anime'
                ? animeRoot
                : picker === 'game'
                  ? gameRoot
                  : picker === 'cache'
                    ? cacheDir
                    : picker === 'plugins'
                      ? pluginsDir || (settings?.defaultPluginsDir ?? '')
                      : (plugins?.root ?? '')
            }
            onPick={(path) => handlePick(picker, path)}
            onClose={() => setPicker(null)}
          />
        )
      )}
    </div>
  );
}
