import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

export type ItemKind = 'anime' | 'game';

export interface MediaSummary {
  fileCount: number;
  episodeHint: string | null;
}

export interface MediaFile {
  name: string;
  sizeBytes: number;
  episode: number | null;
  /** 来源目录（相对条目目录），直属文件缺省。 */
  group?: string | null;
}

export interface ItemSummary {
  id: string;
  kind: ItemKind;
  title: string;
  bangumiSubjectId: number | null;
  folderPath: string;
  createdAt: string;
  updatedAt: string;
  /** Bangumi 收藏状态 1-5，null 表示未设置。 */
  collectionStatus: number | null;
  /** Bangumi 评分 0-10，null 表示未评分。 */
  rating: number | null;
  collectionUpdatedAt: string | null;
  media: MediaSummary;
}

export interface ItemDetail {
  item: ItemSummary;
  files: MediaFile[];
}

export interface ScanReport {
  kind: ItemKind;
  root: string;
  foldersSeen: number;
  created: number;
  updated: number;
  unchanged: number;
}

export interface ScanSummary {
  anime: ScanReport | null;
  game: ScanReport | null;
  totalCreated: number;
  totalUpdated: number;
  totalUnchanged: number;
  messages: string[];
}

export interface Settings {
  animeRoot: string | null;
  gameRoot: string | null;
  metadataCacheDir: string;
  defaultMetadataCacheDir: string;
  metadataCacheDirIsDefault: boolean;
  playerPath: string | null;
  /** 生效中的播放器参数模板，含 {file} 与 {seconds} 占位符。 */
  playerArgs: string;
  bangumiClientId: string | null;
  bangumiHasSecret: boolean;
  /** 当前构建内置了 Bangumi 应用：凭据不可见、不可修改。 */
  bangumiBuiltin: boolean;
  bangumiRedirectUri: string | null;
  bangumiProxy: string | null;
  bangumiDefaultRedirectUri: string;
  pluginsDir: string | null;
  defaultPluginsDir: string;
}

export interface SettingsInput {
  animeRoot: string | null;
  gameRoot: string | null;
  metadataCacheDir: string | null;
  playerPath: string | null;
  playerArgs: string | null;
  pluginsDir: string | null;
}

export interface DirEntry {
  name: string;
  path: string;
}

export interface DirListing {
  path: string | null;
  parent: string | null;
  entries: DirEntry[];
}

export interface FileEntry {
  name: string;
  path: string;
  sizeBytes: number;
}

/** .gamelauncher.json 的前端视图。 */
export interface GameBinding {
  exePath: string;
  resolvedExePath: string;
  exeExists: boolean;
  args: string[];
  workingDir: string;
  lastLaunchedAt: string | null;
  lastPid: number | null;
}

export interface GameStatus {
  runningPid: number | null;
  binding: GameBinding | null;
}

export interface RunningGame {
  itemId: string;
  title: string;
  pid: number;
  exePath: string;
  startedAt: string;
}

export interface ActiveSession {
  itemId: string;
  episode: number;
  fileName: string;
  pid: number;
  startedAt: string;
  /** 当前播放位置（秒）：能读到播放器上报时为真实值。 */
  positionSeconds: number;
  startSeconds: number;
}

export interface PlayingSession {
  itemId: string;
  episode: number;
  fileName: string;
  uri: string;
  pid: number;
  startedAt: string;
  /** 本次会话的续播起点（秒）。 */
  startSeconds: number;
  /** 起点是否真的生效；false 表示播放器不支持跳转，实际从头播放。 */
  resumeApplied: boolean;
}

export interface ProgressRow {
  episode: number;
  watchedSeconds: number;
  source: string;
  updatedAt: string;
}

export interface AppEventEnvelope {
  topic: string;
  payload: Record<string, unknown>;
  emittedAt: string;
}

/** 后端 AppError 的序列化形态。 */
interface AppErrorPayload {
  code: string;
  message: string;
}

export class ApiError extends Error {
  readonly code: string;

  constructor(code: string, message: string) {
    super(message);
    this.name = 'ApiError';
    this.code = code;
  }
}

function isAppErrorPayload(value: unknown): value is AppErrorPayload {
  if (typeof value !== 'object' || value === null) return false;
  const candidate = value as { code?: unknown; message?: unknown };
  return typeof candidate.code === 'string' && typeof candidate.message === 'string';
}

function normalizeError(error: unknown): ApiError {
  if (error instanceof ApiError) return error;
  if (isAppErrorPayload(error)) return new ApiError(error.code, error.message);
  if (typeof error === 'string') return new ApiError('unknown', error);
  if (error instanceof Error) return new ApiError('unknown', error.message);
  return new ApiError('unknown', '未知错误');
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (error) {
    throw normalizeError(error);
  }
}

export function getSettings(): Promise<Settings> {
  return call<Settings>('get_settings');
}

export function saveSettings(input: SettingsInput): Promise<Settings> {
  return call<Settings>('save_settings', { input });
}

export function scanLibrary(): Promise<ScanSummary> {
  return call<ScanSummary>('scan_library');
}

export function listItems(kind: ItemKind | null): Promise<ItemSummary[]> {
  return call<ItemSummary[]>('list_items', { kind });
}

export function getItem(id: string): Promise<ItemDetail> {
  return call<ItemDetail>('get_item', { id });
}

export function bindBangumiSubject(itemId: string, subjectId: number | null): Promise<ItemSummary> {
  return call<ItemSummary>('bind_bangumi_subject', { itemId, subjectId });
}

export function listSubdirs(path: string | null): Promise<DirListing> {
  return call<DirListing>('list_subdirs', { path });
}

export function listExeFiles(path: string): Promise<FileEntry[]> {
  return call<FileEntry[]>('list_exe_files', { path });
}

export function gameStatus(itemId: string): Promise<GameStatus> {
  return call<GameStatus>('game_status', { itemId });
}

export function setGameBinding(itemId: string, exePath: string): Promise<GameStatus> {
  return call<GameStatus>('set_game_binding', { itemId, exePath });
}

export function startGame(itemId: string): Promise<RunningGame> {
  return call<RunningGame>('start_game', { itemId });
}

export function runningGames(): Promise<RunningGame[]> {
  return call<RunningGame[]>('running_games');
}

export function activeSessions(): Promise<ActiveSession[]> {
  return call<ActiveSession[]>('active_sessions');
}

export function playEpisode(itemId: string, fileName: string): Promise<PlayingSession> {
  return call<PlayingSession>('play_episode', { itemId, fileName });
}

/** 播放指定集之后的下一集；已是最后一集时返回 null。 */
export function playNextEpisode(itemId: string, episode: number): Promise<PlayingSession | null> {
  return call<PlayingSession | null>('play_next_episode', { itemId, episode });
}

/** 批量取已缓存的封面（key = itemId）；缺的会在后台补齐并发事件。 */
export function itemCovers(ids: string[]): Promise<Record<string, string>> {
  return call<Record<string, string>>('item_covers', { ids });
}

/** 条目封面（data URL）；未绑定 Bangumi 或没有封面时返回 null。 */
export function itemCover(itemId: string): Promise<string | null> {
  return call<string | null>('item_cover', { itemId });
}

export function listProgress(itemId: string): Promise<ProgressRow[]> {
  return call<ProgressRow[]>('list_progress', { itemId });
}

export function setProgress(
  itemId: string,
  episode: number,
  watchedSeconds: number,
): Promise<ProgressRow> {
  return call<ProgressRow>('set_progress', { itemId, episode, watchedSeconds });
}

// ---- M3 同步 ----

/** 收藏状态 / 评分。 */
export interface CollectionInfo {
  itemId: string;
  collectionStatus: number | null;
  rating: number | null;
  collectionUpdatedAt: string | null;
}

/** 同步队列一项。 */
export interface SyncQueueItem {
  id: string;
  itemId: string | null;
  itemTitle: string | null;
  op: string;
  status: 'pending' | 'sending' | 'failed' | 'done';
  attempts: number;
  nextRetryAt: string | null;
  createdAt: string;
}

export interface SyncStats {
  authorized: boolean;
  pending: number;
  failed: number;
  done: number;
  unresolvedConflicts: number;
}

/** 冲突一项；field 形如 collection_status / rating / progress:5。 */
export interface ConflictItem {
  id: string;
  itemId: string | null;
  itemTitle: string | null;
  field: string;
  localValue: string | null;
  cloudValue: string | null;
  localTs: string | null;
  cloudTs: string | null;
}

export interface PushReport {
  attempted: number;
  succeeded: number;
  failed: number;
}

export interface PullReport {
  scannedItems: number;
  conflicts: number;
  importedCollection: number;
  importedProgress: number;
  messages: string[];
}

export interface ResolveReport {
  resolved: number;
}

export interface MetadataCacheResult {
  subjectId: number;
  cachedPath: string;
}

/** 扫描后自动匹配与首次拉取的结果。 */
export interface AutoMatchReport {
  unboundScanned: number;
  bound: number;
  metadataCached: number;
  pulled: number;
  conflicts: number;
  /** 未能自动绑定的条目及原因。 */
  unmatched: string[];
  messages: string[];
}

export function autoMatch(): Promise<AutoMatchReport> {
  return call<AutoMatchReport>('auto_match');
}

/** Bangumi 搜索结果的一项（手动绑定用）。 */
export interface SubjectSearchHit {
  id: number;
  name: string;
  nameCn: string | null;
  score: number | null;
}

export function searchBangumi(itemId: string, keyword?: string): Promise<SubjectSearchHit[]> {
  return call<SubjectSearchHit[]>('search_bangumi', { itemId, keyword: keyword ?? null });
}

// ---- 插件（M4 阶段 1） ----

export interface PluginInfo {
  id: string;
  name: string;
  version: string;
  kind: string;
  apiVersion: string;
  enabled: boolean;
  installed: boolean;
  manifestPath: string;
  dir: string;
  permissions: string[];
  capabilities: string[];
  mount: string | null;
  error: string | null;
}

export interface PluginOverview {
  root: string;
  plugins: PluginInfo[];
}

export function pluginList(): Promise<PluginOverview> {
  return call<PluginOverview>('plugin_list');
}

export function pluginInstall(path: string): Promise<PluginOverview> {
  return call<PluginOverview>('plugin_install', { path });
}

export function pluginSetEnabled(id: string, enabled: boolean): Promise<PluginOverview> {
  return call<PluginOverview>('plugin_set_enabled', { id, enabled });
}

export function pluginUninstall(id: string): Promise<PluginOverview> {
  return call<PluginOverview>('plugin_uninstall', { id });
}

/** UI 插件入口源码。 */
export interface PluginEntry {
  id: string;
  apiVersion: string;
  mount: string;
  source: string;
}

export function pluginUiEntry(id: string): Promise<PluginEntry> {
  return call<PluginEntry>('plugin_ui_entry', { id });
}

export function pluginReadItems(id: string): Promise<ItemSummary[]> {
  return call<ItemSummary[]>('plugin_read_items', { id });
}

export function pluginEmit(id: string, name: string, payload?: unknown): Promise<void> {
  return call<void>('plugin_emit', { id, name, payload: payload ?? null });
}

export function pluginUiUnloaded(id: string): Promise<void> {
  return call<void>('plugin_ui_unloaded', { id });
}

/** 能力插件一次执行的结果。 */
export interface PluginInvocation {
  id: string;
  output: unknown;
  logs: string[];
  durationMs: number;
  fuelUsed: number;
}

/** 在 WASM 沙箱里执行能力插件；input 必须是可 JSON 序列化的值。 */
export function pluginInvoke(id: string, input: unknown): Promise<PluginInvocation> {
  return call<PluginInvocation>('plugin_invoke', { id, input: input ?? null });
}

/** 删除资源的执行结果。 */
export interface DeleteReport {
  itemId: string;
  title: string;
  /** `trash` 或 `permanent`。 */
  mode: string;
  folderPath: string;
  filesRemoved: boolean;
}

/**
 * 删除条目资源：默认移入回收站；`permanent` 必须传与条目标题一致的 confirm。
 */
export function deleteItem(
  id: string,
  mode: 'trash' | 'permanent',
  confirm?: string,
): Promise<DeleteReport> {
  return call<DeleteReport>('delete_item', { id, mode, confirm: confirm ?? null });
}

/** 批量刷新元数据的结果。 */
export interface BatchRefreshResult {
  refreshed: number;
  skipped: number;
  failed: number;
  messages: string[];
}

/** 批量刷新已绑定条目的元数据（未绑定的会被跳过）。 */
export function refreshMetadataBatch(ids: string[]): Promise<BatchRefreshResult> {
  return call<BatchRefreshResult>('refresh_metadata_batch', { ids });
}

/** 批量移入回收站的结果。 */
export interface BatchDeleteResult {
  deleted: number;
  failed: number;
  messages: string[];
}

/** 批量移入回收站；永久删除只能逐条确认。 */
export function trashItems(ids: string[]): Promise<BatchDeleteResult> {
  return call<BatchDeleteResult>('trash_items', { ids });
}

/** 在资源管理器里打开条目目录；传 fileName 时改为选中该文件。 */
export function openInExplorer(itemId: string, fileName?: string): Promise<void> {
  return call<void>('open_in_explorer', { itemId, fileName: fileName ?? null });
}

/** 日志目录与文件清单（设置页展示）。 */
export interface LogFile {
  name: string;
  sizeBytes: number;
  modifiedAt: string;
}

export interface LogLocation {
  dir: string;
  currentFile: string;
  maxBytes: number;
  keepFiles: number;
  files: LogFile[];
}

export function logLocation(): Promise<LogLocation> {
  return call<LogLocation>('log_location');
}

/** 拖入导入成功的一项。 */
export interface ImportedItem {
  source: string;
  target: string;
  name: string;
  kind: ItemKind;
}

export interface ImportReport {
  imported: ImportedItem[];
  failed: string[];
  skipped: string[];
}

/** 导入拖进窗口的目录 / zip；返回逐项结果。 */
export function importDrop(paths: string[]): Promise<ImportReport> {
  return call<ImportReport>('import_drop', { paths });
}

/** 搜索结果的条目概览（右侧预览面板）。 */
export interface SubjectPreviewTag {
  name: string;
  count: number;
}

export interface SubjectPreviewRelated {
  id: number;
  name: string;
  nameCn: string;
  relation: string;
  subjectType: number;
}

export interface SubjectPreview {
  id: number;
  name: string;
  nameCn: string;
  summary: string;
  subjectType: number;
  date: string | null;
  eps: number | null;
  score: number | null;
  rank: number | null;
  ratingTotal: number;
  tags: SubjectPreviewTag[];
  related: SubjectPreviewRelated[];
  cover: string | null;
  url: string;
}

/** 取某个 Bangumi 条目的概览（含封面、标签、关联条目）。 */
export function bangumiSubjectPreview(subjectId: number): Promise<SubjectPreview> {
  return call<SubjectPreview>('bangumi_subject_preview', { subjectId });
}

/** 用系统默认浏览器打开链接。 */
export function openExternal(url: string): Promise<void> {
  return call<void>('open_external', { url });
}

/** 当前登录的 Bangumi 账号。 */
export interface AccountInfo {
  authorized: boolean;
  id: number | null;
  username: string | null;
  nickname: string | null;
  avatarUrl: string | null;
  expiresAt: string | null;
}

export function bangumiAccount(): Promise<AccountInfo> {
  return call<AccountInfo>('bangumi_account');
}

/** 丢弃一条待推送记录（只删本地队列）。 */
export function discardSyncEntry(id: string): Promise<boolean> {
  return call<boolean>('discard_sync_entry', { id });
}

/** 立即拉取单个条目的云端收藏与进度。 */
export function pullItemNow(itemId: string): Promise<ItemSummary> {
  return call<ItemSummary>('pull_item_now', { itemId });
}

/** 元数据缓存占用。 */
export interface CacheStats {
  dir: string;
  subjectDirs: number;
  fileCount: number;
  totalBytes: number;
}

export function metadataCacheStats(): Promise<CacheStats> {
  return call<CacheStats>('metadata_cache_stats');
}

export interface CacheClearResult {
  removedDirs: number;
  freedBytes: number;
}

export function clearMetadataCache(): Promise<CacheClearResult> {
  return call<CacheClearResult>('clear_metadata_cache');
}

/**
 * 清空条目记录（可按类型）。
 *
 * 只删数据库里的条目信息（进度、绑定、同步队列、冲突一并级联清理），
 * **不会动磁盘上的任何文件**。返回删除的条目数。
 */
export function clearItems(kind?: ItemKind | null): Promise<number> {
  return call<number>('clear_items', { kind: kind ?? null });
}

/** 打开日志目录（资源管理器）。 */
export function openLogDir(): Promise<void> {
  return call<void>('open_log_dir');
}

/** 前端就绪打点：脚本能跑通就会调用一次，便于从日志判断是否白屏。 */
export function uiReady(): Promise<void> {
  return call<void>('ui_ready');
}

export function setCollection(
  itemId: string,
  collectionStatus: number | null,
  rating: number | null,
): Promise<CollectionInfo> {
  return call<CollectionInfo>('set_collection', { itemId, collectionStatus, rating });
}

export function listSyncQueue(): Promise<SyncQueueItem[]> {
  return call<SyncQueueItem[]>('list_sync_queue');
}

export function syncStats(): Promise<SyncStats> {
  return call<SyncStats>('sync_stats');
}

export function pushPending(limit?: number): Promise<PushReport> {
  return call<PushReport>('push_pending', { limit });
}

export function syncPull(): Promise<PullReport> {
  return call<PullReport>('sync_pull');
}

export function listConflicts(): Promise<ConflictItem[]> {
  return call<ConflictItem[]>('list_conflicts');
}

export function resolveConflicts(
  conflictIds: string[],
  choice: 'local' | 'cloud',
): Promise<ResolveReport> {
  return call<ResolveReport>('resolve_conflicts', { conflictIds, choice });
}

export function cacheMetadata(itemId: string): Promise<MetadataCacheResult> {
  return call<MetadataCacheResult>('cache_metadata', { itemId });
}

// ---- Bangumi OAuth ----

export interface BangumiStatus {
  authorized: boolean;
  expiresAt: string | null;
  hasCredentials: boolean;
  clientId: string | null;
  redirectUri: string | null;
  /** 凭据来自软件内置应用（用户没有填写自己的）。 */
  usingBuiltinApp: boolean;
}

export interface BangumiConfigInput {
  clientId: string;
  clientSecret: string;
  redirectUri: string;
  /** 形如 http://127.0.0.1:7890；留空则自动探测系统代理。 */
  proxy: string;
}

export function bangumiStatus(): Promise<BangumiStatus> {
  return call<BangumiStatus>('bangumi_status');
}

export function saveBangumiConfig(input: BangumiConfigInput): Promise<BangumiStatus> {
  return call<BangumiStatus>('save_bangumi_config', { input });
}

/** 打开浏览器完成授权，最长等待 180 秒的回调。 */
export function bangumiAuthorize(): Promise<BangumiStatus> {
  return call<BangumiStatus>('bangumi_authorize');
}

export function bangumiLogout(): Promise<BangumiStatus> {
  return call<BangumiStatus>('bangumi_logout');
}

/** 兜底：粘贴浏览器回调地址（含 code）或授权码完成授权。 */
export function bangumiCompleteManual(callback: string): Promise<BangumiStatus> {
  return call<BangumiStatus>('bangumi_complete_manual', { callback });
}

/** 主干道事件通道，主题在信封的 topic 字段里。 */
export const EVENT_CHANNEL = 'lagm://event';

export async function subscribeAppEvents(
  handler: (envelope: AppEventEnvelope) => void,
): Promise<() => void> {
  return listen<AppEventEnvelope>(EVENT_CHANNEL, (event) => {
    handler(event.payload);
  });
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let value = bytes / 1024;
  let index = 0;
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024;
    index += 1;
  }
  return `${value.toFixed(value >= 100 ? 0 : 1)} ${units[index]}`;
}

/** 秒数转 mm:ss / h:mm:ss，用于进度展示。 */
export function formatDuration(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const rest = total % 60;
  if (hours > 0) {
    return `${hours}:${String(minutes).padStart(2, '0')}:${String(rest).padStart(2, '0')}`;
  }
  return `${minutes}:${String(rest).padStart(2, '0')}`;
}
