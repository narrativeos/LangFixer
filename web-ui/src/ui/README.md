# ui/ 原语层契约

替换 Element Plus 的自建原语。迁移视图时的硬规则:

- **控件高度只用 token**(`--h-control` 28px),不得引入 EP 默认 32/40px。
- **红色 = 错误/破坏性,仅此一种含义**;层级与来源标签一律 `LTag tone="neutral"`。
- **对话框配额只有三处**(发布确认 / 删资源包 / 回滚确认);其余场景用 LSection 分节、
  行内展开、LPopover、Toast。常规成功反馈用 toast,禁弹窗。
- **空态与错误必须带下一步指引**(LEmpty 的 hint/action;toast 文案说清发生了什么)。
- **词汇只出自 `lib/labels.ts`**(模式名 modeLabel、层级名 plainLayerLabel、错误类型
  errorTypeLabel),不新造话术;CTA 动词与结果 toast 同词。
- 替换时**保持组件文件名、props、emits 不变**,父组件零改动。

## 组件

| 组件 | 契约 | 替代 |
|---|---|---|
| `LButton` | `variant: 'default'|'primary'|'ghost'|'danger'`,`size: 'md'|'sm'`,原生 button | el-button |
| `LSegmented` | `v-model`,`options: {value,label}[]`,role=tablist | el-radio-group / el-tabs 顶部分段 |
| `LToolbar` | 插槽默认;内含 `.l-spacer` 撑开 | app-header |
| `LSidebar` | `sections: {title?, items: {id,label,badge?}[]}[]`,`v-model` 选中 id | 顶栏导航 |
| `LSection` | `title`,`desc?`,插槽 `#actions` + 默认 | el-card 容器 |
| `LTag` | `tone: 'neutral'|'ok'|'warn'|'danger'|'accent'` | el-tag |
| `LDot` | `tone: 'ok'|'warn'|'danger'|'muted'` | 手写状态点 |
| `LInput` `LSelect` `LSwitch` `LCheckbox` | v-model;原生元素 + token 皮肤 | el-input/select/switch/checkbox |
| `LTable` | `columns: {key,label,sortable?}[]`,`rows`,`rowKey`,多选 `selection`、`v-model:selected`;事件 `row-click` `row-contextmenu`;具名插槽 `col-<key>` 自定义单元格 | el-table |
| `LDialog` | `v-model:visible`,`title`,插槽默认 + `#footer`;原生 dialog(Esc 即关) | el-dialog(仅配额内) |
| `LPopover` | 插槽 `#trigger` + 默认内容,`align: 'start'|'end'`,Esc/外点关闭 | el-popover/el-tooltip(浮层) |
| `LProgress` | `value: 0..1` | el-progress |
| `LEmpty` | `text`,`hint?`,插槽 `#action` | el-empty |
| `toast(message, tone?, action?)` | 底部 toast;`action: {label,onClick}` 撤销位 | ElMessage/ElMessageBox(提示类) |

toast 由 App 挂的 `<LToaster/>` 渲染;组件内 `import { toast } from '../ui/toast'`。
