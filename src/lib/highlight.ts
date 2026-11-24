import hljs from 'highlight.js/lib/core';
import type { LanguageFn } from 'highlight.js';
import bash from 'highlight.js/lib/languages/bash';
import c from 'highlight.js/lib/languages/c';
import cmake from 'highlight.js/lib/languages/cmake';
import cpp from 'highlight.js/lib/languages/cpp';
import csharp from 'highlight.js/lib/languages/csharp';
import css from 'highlight.js/lib/languages/css';
import diff from 'highlight.js/lib/languages/diff';
import dockerfile from 'highlight.js/lib/languages/dockerfile';
import go from 'highlight.js/lib/languages/go';
import graphql from 'highlight.js/lib/languages/graphql';
import groovy from 'highlight.js/lib/languages/groovy';
import ini from 'highlight.js/lib/languages/ini';
import java from 'highlight.js/lib/languages/java';
import javascript from 'highlight.js/lib/languages/javascript';
import json from 'highlight.js/lib/languages/json';
import kotlin from 'highlight.js/lib/languages/kotlin';
import less from 'highlight.js/lib/languages/less';
import lua from 'highlight.js/lib/languages/lua';
import makefile from 'highlight.js/lib/languages/makefile';
import markdown from 'highlight.js/lib/languages/markdown';
import nginx from 'highlight.js/lib/languages/nginx';
import perl from 'highlight.js/lib/languages/perl';
import php from 'highlight.js/lib/languages/php';
import powershell from 'highlight.js/lib/languages/powershell';
import protobuf from 'highlight.js/lib/languages/protobuf';
import python from 'highlight.js/lib/languages/python';
import ruby from 'highlight.js/lib/languages/ruby';
import rust from 'highlight.js/lib/languages/rust';
import scss from 'highlight.js/lib/languages/scss';
import sql from 'highlight.js/lib/languages/sql';
import swift from 'highlight.js/lib/languages/swift';
import typescript from 'highlight.js/lib/languages/typescript';
import xml from 'highlight.js/lib/languages/xml';
import yaml from 'highlight.js/lib/languages/yaml';
import 'highlight.js/styles/github-dark.css';

const LANGS: Record<string, LanguageFn> = {
  bash, c, cmake, cpp, csharp, css, diff, dockerfile, go, graphql, groovy,
  ini, java, javascript, json, kotlin, less, lua, makefile, markdown, nginx,
  perl, php, powershell, protobuf, python, ruby, rust, scss, sql, swift,
  typescript, xml, yaml,
};
for (const [name, fn] of Object.entries(LANGS)) hljs.registerLanguage(name, fn);

const BY_EXT: Record<string, string> = {
  js: 'javascript', mjs: 'javascript', cjs: 'javascript', jsx: 'javascript',
  ts: 'typescript', mts: 'typescript', cts: 'typescript', tsx: 'typescript',
  json: 'json', jsonc: 'json', geojson: 'json',
  py: 'python', pyw: 'python',
  rs: 'rust',
  go: 'go',
  c: 'c', h: 'c',
  cc: 'cpp', cpp: 'cpp', cxx: 'cpp', hpp: 'cpp', hh: 'cpp', hxx: 'cpp',
  cs: 'csharp',
  java: 'java', kt: 'kotlin', kts: 'kotlin', gradle: 'groovy',
  swift: 'swift',
  php: 'php',
  rb: 'ruby', rake: 'ruby', gemspec: 'ruby',
  pl: 'perl', pm: 'perl',
  lua: 'lua',
  sh: 'bash', bash: 'bash', zsh: 'bash', ksh: 'bash',
  ps1: 'powershell', psm1: 'powershell',
  sql: 'sql',
  yml: 'yaml', yaml: 'yaml',
  toml: 'ini', ini: 'ini', conf: 'ini', cfg: 'ini', properties: 'ini',
  xml: 'xml', html: 'xml', htm: 'xml', xhtml: 'xml', vue: 'xml',
  css: 'css', scss: 'scss', sass: 'scss', less: 'less',
  md: 'markdown', markdown: 'markdown', mdown: 'markdown',
  diff: 'diff', patch: 'diff',
  dockerfile: 'dockerfile',
  mk: 'makefile', cmake: 'cmake',
  gql: 'graphql', graphql: 'graphql',
  proto: 'protobuf',
};

const BY_NAME: Record<string, string> = {
  dockerfile: 'dockerfile',
  containerfile: 'dockerfile',
  makefile: 'makefile',
  gnumakefile: 'makefile',
  'cmakelists.txt': 'cmake',
  'cargo.toml': 'ini',
  'cargo.lock': 'ini',
  'package.json': 'json',
  'package-lock.json': 'json',
  'tsconfig.json': 'json',
  'nginx.conf': 'nginx',
  '.gitignore': 'bash',
  '.dockerignore': 'bash',
  '.npmignore': 'bash',
  '.env': 'bash',
  '.editorconfig': 'ini',
  '.npmrc': 'ini',
  '.gitconfig': 'ini',
  '.bashrc': 'bash',
  '.zshrc': 'bash',
  '.profile': 'bash',
  '.bash_profile': 'bash',
};

export function languageFor(path: string): string | null {
  const base = (path.split('/').pop() || path).toLowerCase();
  if (BY_NAME[base]) return BY_NAME[base];
  if (base.startsWith('dockerfile.')) return 'dockerfile';
  if (base.startsWith('makefile.')) return 'makefile';
  const dot = base.lastIndexOf('.');
  if (dot <= 0) return null;
  return BY_EXT[base.slice(dot + 1)] ?? null;
}

export function escapeHtml(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

export function highlight(code: string, path: string): string | null {
  const lang = languageFor(path);
  if (!lang || !hljs.getLanguage(lang)) return null;
  try {
    return hljs.highlight(code, { language: lang, ignoreIllegals: true }).value;
  } catch {
    return null;
  }
}