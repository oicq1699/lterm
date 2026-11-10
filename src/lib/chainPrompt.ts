/** get_proxy_chain 返回的每一跳，用于连接前收集口令 */
export type ChainHop = {
  id: string; name: string; host: string; port: number; username: string;
  auth_method: 'password' | 'key' | 'agent';
  key_path?: string | null;
  key_passphrase_required?: boolean;
  /** 后端已有保存的口令，连接时无需再问 */
  has_saved_secret?: boolean;
};

/**
 * 这一跳需要人工输入什么，返回提示语；null 表示不用问。
 * 私钥只在文件确实带口令时才问，否则明文密钥每次连接都得多点一次确定。
 */
export function hopPromptLabel(h: ChainHop, passwordWord = '密码'): string | null {
  if (h.auth_method === 'password') return `${passwordWord} (${h.name} · ${h.username}@${h.host}:${h.port})`;
  if (h.auth_method === 'key' && h.key_passphrase_required) {
    return `私钥口令 (${h.name} · ${h.key_path ?? ''})`;
  }
  return null;
}
