import { Channel, invoke } from '@tauri-apps/api/core';
import { save } from '@tauri-apps/plugin-dialog';

export interface Conversation {
  id: number;
  title: string;
  model: string;
  createdAt: number;
  updatedAt: number;
  pinned: boolean;
  deletedAt: number | null;
}
export interface SearchHit extends Conversation {
  /** matching text; hits wrapped in \u0002 … \u0003 */
  snippet: string;
}
export interface AttachmentMeta {
  id: number;
  kind: 'image' | 'pdf';
  name: string;
  mime: string;
  pages: number | null;
  chars: number;
  truncated: boolean;
}
export interface Stats {
  model?: string;
  numCtx?: number;
  tokS?: number;
  evalCount?: number;
  promptEvalCount?: number;
  elapsedMs?: number;
  error?: string;
  searchError?: string;
}
export interface Source {
  title: string;
  url: string;
}
export type Status = 'complete' | 'streaming' | 'stopped' | 'error' | 'interrupted';
export interface Message {
  id: number;
  conversationId: number;
  role: 'user' | 'assistant';
  content: string;
  thinking: string;
  status: Status;
  createdAt: number;
  stats: Stats | null;
  sources: Source[] | null;
  attachments: AttachmentMeta[];
}
export interface NewAttachment {
  kind: 'image' | 'pdf';
  name: string;
  mime: string;
  dataB64?: string;
  text?: string;
  pages?: number;
  truncated?: boolean;
}
export type ChatEvent =
  | { event: 'searching' }
  | { event: 'sources'; sources: Source[] }
  | { event: 'searchError'; message: string }
  | { event: 'delta'; content: string; thinking: string }
  | { event: 'done'; status: 'complete' | 'stopped'; stats: Stats }
  | { event: 'error'; message: string; stats: Stats };
export interface SendIds {
  conversationId: number;
  userMessageId: number;
  assistantMessageId: number;
  title: string;
}
export interface SendArgs {
  conversationId: number | null;
  model: string;
  text: string;
  attachments: NewAttachment[];
  numCtx: number;
  web: boolean;
}
export interface EnsureInfo {
  state: 'running' | 'started' | 'failed';
  message: string;
}

export const api = {
  settings: () => invoke<Record<string, string>>('get_settings'),
  setSetting: (key: string, value: string) => invoke<void>('set_setting', { key, value }),
  ollamaEnsure: () => invoke<EnsureInfo>('ollama_ensure'),
  listModels: () => invoke<string[]>('list_models'),
  modelVision: (model: string) => invoke<boolean>('model_vision', { model }),

  send(args: SendArgs, onEvent: (e: ChatEvent) => void): Promise<SendIds> {
    const ch = new Channel<ChatEvent>();
    ch.onmessage = onEvent;
    return invoke<SendIds>('send_message', { args, onEvent: ch });
  },
  cancel: (messageId: number) => invoke<void>('cancel_generation', { messageId }),

  conversations: () => invoke<Conversation[]>('list_conversations'),
  trash: () => invoke<Conversation[]>('list_trash'),
  searchConversations: (query: string) => invoke<SearchHit[]>('search_conversations', { query }),
  load: (id: number) => invoke<{ conversation: Conversation; messages: Message[] }>('load_conversation', { id }),
  rename: (id: number, title: string) => invoke<void>('rename_conversation', { id, title }),
  pin: (id: number, pinned: boolean) => invoke<void>('pin_conversation', { id, pinned }),
  remove: (id: number) => invoke<void>('delete_conversation', { id }),
  restore: (id: number) => invoke<void>('restore_conversation', { id }),
  purge: (id: number) => invoke<void>('purge_conversation', { id }),
  emptyTrash: () => invoke<number>('empty_trash'),
  attachmentBytes: (id: number) => invoke<ArrayBuffer>('attachment_bytes', { id }),

  saveText: (path: string, content: string) => invoke<void>('save_text_file', { path, content }),
  exportAllJson: (path: string) => invoke<void>('export_all_json', { path }),
  backupDatabase: (path: string) => invoke<void>('backup_database', { path }),
  dataLocation: () => invoke<string>('data_location'),
  openExternal: (url: string) => invoke<void>('open_external', { url }),

  /** Native save dialog; null when cancelled. */
  pickSavePath: (defaultPath: string, filterName: string, ext: string) =>
    save({ defaultPath, filters: [{ name: filterName, extensions: [ext] }] }),
};

export function errMsg(e: unknown): string {
  return typeof e === 'string' ? e : e instanceof Error ? e.message : JSON.stringify(e);
}
