# 项目约定

## 发布

- **打 tag / 发布 GitHub Release 必须先经用户明确确认。** 不要在未获确认时执行 `git tag`、`git push origin <tag>`、删除或重建标签，也不要触发 Actions 出包。
- 日常提交推送到 `master` 不受此限制（不会触发出包，工作流只在 tag 或手动 Run workflow 时运行）。
