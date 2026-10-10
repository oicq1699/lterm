# 项目约定

## 本地构建

- **每次改完代码，本地只构建 Linux 便携二进制**：`npm run tauri build -- --no-bundle`，产物 `src-tauri/target/release/lterm`。
- 不在本地打 Windows 包、不生成任何安装包（deb/rpm/AppImage/msi）；Windows 产物一律交给 GitHub Actions。
- 构建完把二进制复制一份到 `/root/workspace/lterm-linux-x64-portable`，供用户直接取用验证。

## 发布

- **打 tag / 发布 GitHub Release 必须先经用户明确确认。** 不要在未获确认时执行 `git tag`、`git push origin <tag>`、删除或重建标签，也不要触发 Actions 出包。
- 日常提交推送到 `master` 不受此限制（不会触发出包，工作流只在 tag 或手动 Run workflow 时运行）。
