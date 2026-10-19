# Lterm 一个简单的ssh终端工具

一直没有一款自己用的称心的ssh工具，现在ai功能这么强，所以直接用ai写了一款，代码全由ai生成。

## RDP（远程桌面）需要系统客户端

RDP 主机不在本程序内实现协议，而是调用系统的远程桌面客户端，用之前请先确认已安装：

- **Windows**：自带 `mstsc.exe`，无需安装；密码由 mstsc 自己的弹窗输入。
- **Linux Mint / Ubuntu**：需要 xfreerdp —— `sudo apt install freerdp3-x11`（Mint 21 那代仓库里是 `freerdp2-x11`，命令名为 `xfreerdp`）。没装时连接会在界面上给出同样的安装提示。

在服务器表单里把协议选成 **RDP 远程桌面**（端口默认 3389）即可，桌面在独立窗口打开，不占标签页；支持把本地目录共享到远程、剪贴板互通和全屏。默认证书策略是首次信任（TOFU），自签或换过证书的机器请勾上表单里的**忽略服务端证书**。连不上老服务器（Windows 2003/2008）时，把「兼容」档位从"自动协商"换成"强制 NLA""仅 TLS"或"老式 RDP"再试。

## Tauri + SvelteKit + TypeScript

This template should help get you started developing with Tauri, SvelteKit and TypeScript in Vite.

## Recommended IDE Setup

[VS Code](https://code.visualstudio.com/) + [Svelte](https://marketplace.visualstudio.com/items?itemName=svelte.svelte-vscode) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer).
