<script setup>
import Icon from "../components/Icon.vue";
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import { animate, stagger } from "animejs";
import * as api from "../api";
import { KIND_LABELS, batchNaming, paddedSeq, pickDefaultQuality, singleNaming } from "../download-request.js";
import StepHeader from "../components/StepHeader.vue";

const props = defineProps({
  login: { type: Object, required: true },
  settings: { type: Object, default: null },
  /// 内容库点「去解析」时带过来的来源：{ url, stamp }，看到就自动解析
  pendingSource: { type: Object, default: null },
});
const emit = defineEmits(["toast", "goto"]);

const text = ref("");
const parsing = ref(false);

/** 操作说明：默认收起，保持首屏聚焦；展开内容见模板 help-body */
const showHelp = ref(false);

/** 主按钮磁吸：轻微朝鼠标偏移（±4px），移开弹回。只动 transform。 */
const magnetWrap = ref(null);
function magnetMove(event) {
  if (matchMedia("(prefers-reduced-motion: reduce)").matches) return;
  const el = magnetWrap.value;
  const r = el.getBoundingClientRect();
  const dx = ((event.clientX - r.left) / r.width - 0.5) * 8;
  const dy = ((event.clientY - r.top) / r.height - 0.5) * 6;
  el.style.transform = `translate(${dx.toFixed(1)}px, ${dy.toFixed(1)}px)`;
}
function magnetLeave() {
  if (magnetWrap.value) magnetWrap.value.style.transform = "";
}
const done = ref(0);
const total = ref(0);
const items = ref([]);

/** 本次解析被跳过的重复来源，列在解析结果里 */
const parseSkipped = ref([]);
const selected = ref(new Set());
const okItems = computed(() => items.value.filter((item) => item.ok));
const failedItems = computed(() => items.value.filter((item) => !item.ok));

/** 已经解析出来的来源条数：单条微博算 1，用户主页算已加载条数 */
function sourceCount(item) {
  return item.probe.kind === "post" ? 1 : item.probe.items.length;
}

function itemKey(item) {
  return item.probe ? `post:${item.probe.bid || item.input}` : item.input;
}

const steps = computed(() => {
  return [
    {
      index: 1,
      title: "解析来源",
      hint: parsing.value ? `解析中 ${done.value}/${total.value}` : "输入链接",
      // 状态跟着当前所在页面走：在选择页时第 1 步才算完成
      state: view.value === "select" ? "done" : "active",
      to: view.value === "select" ? "input" : "",
    },
    {
      index: 2,
      title: "选择内容",
      hint: view.value === "select"
        ? activeSource.value?.probe.kind === "post"
          ? "确认内容"
          : `已选 ${selectedCount.value} / 已加载 ${loadedCount.value}`
        : okItems.value.length
          ? `${okItems.value.length} 个来源待选择`
          : "等待解析",
      state: view.value === "select" ? "active" : "idle",
      // 有解析结果时第 2 步才可跳
      to: view.value !== "select" && items.value.length && !parsing.value ? "select" : "",
    },
  ];
});

// 支持的来源；图标用 iconfont 图标名（src/icons.js）
const sources = [
  { label: "单条微博", icon: "link" },
  { label: "视频微博", icon: "play" },
  { label: "图文/多图", icon: "photo" },
  { label: "用户主页", icon: "user" },
  { label: "我的收藏", icon: "star" },
  { label: "多行批量", icon: "listLine" },
];

/** Unix 秒 → 本地日期文本（表格时间列用，悬停看完整日期） */
function formatTime(unixSecs) {
  if (!unixSecs) return "—";
  const d = new Date(unixSecs * 1000);
  const pad = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/** 行的类型标签：转发 > 音频 > 视频 > N 图 > 纯文字 */
function typeLabel(entry) {
  if (entry.is_retweet) return "转发";
  if (entry.is_audio) return "音频";
  if (entry.has_video) return "视频";
  if (entry.pics > 0) return `${entry.pics} 图`;
  return "文字";
}

async function pasteFromClipboard() {
  const value = await api.readClipboard();
  if (!value) {
    emit("toast", "读取剪贴板失败，请手动粘贴");
    return;
  }
  text.value = text.value.trim() ? `${text.value.trim()}\n${value}` : value;
}

/// 内容库送来的来源：填进输入框、直接解析一次。
/// 用 stamp 判重（同一条点两次也要能重新解析），所以不"吃掉"这个值。
watch(
  () => props.pendingSource?.stamp,
  async (stamp) => {
    const url = props.pendingSource?.url;
    if (!stamp || !url) return;
    text.value = url;
    view.value = "input";
    await nextTick();
    if (!parsing.value) await parse();
  }
);

async function parse() {
  // 同一链接贴两次不必解析两遍；被跳过的记下来，回头列在解析结果里
  const skipped = [];
  const inputs = [];
  const seenLine = new Set();
  for (const raw of text.value.split("\n")) {
    const line = raw.trim();
    if (!line) continue;
    if (seenLine.has(line)) {
      skipped.push({ input: line, reason: "与前面的链接相同" });
      continue;
    }
    seenLine.add(line);
    inputs.push(line);
  }
  if (!inputs.length || parsing.value) return;

  // 按解析节奏分批：批内并发、批间等待、每 N 条休息，降低触发风控的概率
  parsing.value = true;
  items.value = [];
  selected.value = new Set();
  done.value = 0;
  total.value = inputs.length;

  const collected = [];
  const seenKeys = new Map();
  const batch = Math.max(1, props.settings?.parse_batch ?? 8);
  const batchWait = props.settings?.parse_batch_wait_ms ?? 1000;
  const restEvery = Math.max(1, props.settings?.parse_rest_every ?? 100);
  const restMs = props.settings?.parse_rest_ms ?? 3000;

  const probeOne = async (input) => {
    try {
      const probe = await api.probeSource(input);
      // 同一个来源（同一链接/同一博主的另一份入口）只保留第一次
      const key = probe.kind === "post" ? `post:${probe.bid}` : `user:${probe.uid}`;
      if (seenKeys.has(key)) {
        skipped.push({
          input,
          reason: `与「${seenKeys.get(key)}」是同一个来源`,
        });
        done.value += 1;
        return;
      }
      seenKeys.set(key, probe.title || input);
      collected.push({
        input,
        ok: true,
        error: "",
        probe,
        quality: pickDefaultQuality(probe, props.settings),
      });
    } catch (error) {
      collected.push({ input, ok: false, error: String(error) });
    }
    done.value += 1;
    items.value = [...collected];
  };

  for (let start = 0; start < inputs.length; start += batch) {
    await Promise.all(inputs.slice(start, start + batch).map(probeOne));
    const finished = start + batch;
    if (finished >= inputs.length) break;
    if (finished % restEvery === 0 && restMs > 0) {
      await new Promise((r) => setTimeout(r, restMs));
    } else if (batchWait > 0) {
      await new Promise((r) => setTimeout(r, batchWait));
    }
  }

  parsing.value = false;
  parseSkipped.value = [...skipped];

  const failed = collected.filter((item) => !item.ok).length;
  if (failed) {
    emit("toast", `${collected.length - failed} 条解析成功，${failed} 条失败`);
  }
  if (skipped.length) {
    emit("toast", `已跳过 ${skipped.length} 个重复来源，明细见解析结果`);
  }

  // 解析成功的一律进「选择内容」页：单条微博与批量清单都在那里挑
  const firstOk = collected.find((item) => item.ok);
  if (firstOk) openSelect(firstOk.input);
}

function isSelected(row) {
  return selected.value.has(row.key);
}

function toggleEntry(row) {
  const next = new Set(selected.value);
  if (next.has(row.key)) next.delete(row.key);
  else next.add(row.key);
  selected.value = next;
}

// ---------- 选择内容页 ----------
// 解析结果不再塞在输入页里，而是独立一页：表格 + 分批加载。
// 首次解析只给第一页，「继续解析」用后端缓存接着往后拉。
const view = ref("input"); // input | select

// 选择页入场：工具条 + 表格行级联（列表以列表的方式出现）
watch(view, async (v) => {
  if (v !== "select" || matchMedia("(prefers-reduced-motion: reduce)").matches) return;
  await nextTick();
  requestAnimationFrame(() => {
    const bar = document.querySelector(".select-bar");
    const rows = [...document.querySelectorAll(".batch-table tbody tr")].slice(0, 12);
    const targets = bar ? [bar, ...rows] : rows;
    if (targets.length) animate(targets, { opacity: [0, 1], translateY: [8, 0], duration: 420, delay: stagger(28), ease: "outExpo" });
  });
});

// 解析结果入口：结果条目逐个亮起（只在从无到有时演一次）
watch(
  () => items.value.length > 0,
  async (has) => {
    if (!has || matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    await nextTick();
    requestAnimationFrame(() => {
      const cells = [...document.querySelectorAll(".parsed-bar > *")];
      if (cells.length) animate(cells, { opacity: [0, 1], translateY: [6, 0], duration: 340, delay: stagger(40), ease: "outExpo" });
    });
  }
);
const batchInput = ref(""); // 当前查看的来源（继续解析的键）
const batchSize = ref(50); // 一次往后拉多少条
const loadingMore = ref(false);

/** 已解析出的来源：单条微博与用户主页都能在选择页里看 */
const allSources = computed(() => okItems.value);

const activeSource = computed(() => {
  const list = allSources.value;
  return list.find((item) => item.input === batchInput.value) || list[0] || null;
});

/** 选择页要占满高度：顶部工具条与底部统计固定，只有表体滚动 */
const isSelectView = computed(() => view.value === "select" && !!activeSource.value);

/** 当前来源是批量清单（单条微博没有分页） */
const activeIsBatch = computed(() => !!activeSource.value && activeSource.value.probe.kind !== "post");

/** 表头：只有一个来源就显示它自己，多个来源显示来源数 */
const headerTitle = computed(() => {
  if (allSources.value.length === 1) return activeSource.value?.probe.title ?? "";
  const users = allSources.value.filter((item) => item.probe.kind !== "post");
  if (users.length === allSources.value.length) return `${allSources.value.length} 个用户主页`;
  return `${allSources.value.length} 个来源`;
});

const headerTag = computed(() => {
  if (allSources.value.length === 1) return kindLabel(activeSource.value?.probe.kind ?? "");
  return allSources.value.every((item) => item.probe.kind === "post") ? "微博" : "";
});

/** 行悬停显示该行将落盘的文件名（与下载共用后端渲染器） */
function fileNameOf(row) {
  const name = fileNames.value[row.seq - 1];
  return name ? `${row.title}  |  文件名：${name}` : row.title;
}

const loadedCount = computed(() => tableRows.value.length);

/** 微博接口不给总数，只报已加载；拉到底才有准确的总数 */
const loadedHint = computed(() => {
  const probe = activeSource.value?.probe;
  if (!probe) return "";
  if (probe.total && activeIsBatch.value) return `已加载 ${loadedCount.value} / ${probe.total} 项`;
  return `已加载 ${loadedCount.value} 项`;
});

const selectedCount = computed(() => tableRows.value.filter((row) => isSelected(row)).length);
const allLoadedSelected = computed(
  () => loadedCount.value > 0 && selectedCount.value === loadedCount.value
);

/** 全选/全不选只作用于"已加载"的部分，没拉下来的不会被选中 */
function toggleAllLoaded(checked) {
  const next = new Set(selected.value);
  for (const row of tableRows.value) {
    if (checked) next.add(row.key);
    else next.delete(row.key);
  }
  selected.value = next;
}

// ---- 下载设置弹层绑定：多来源清单用共享的一档，单来源用来源自己的 ----
const multiQuality = ref("auto");
const multiImage = ref("large");

const IMAGE_CHOICES = [
  { value: "large", label: "原图（最佳画质）" },
  { value: "thumbnail", label: "压缩图（体积小）" },
];

const dlQuality = computed({
  get: () => (useShared.value ? multiQuality.value : activeSource.value?.quality ?? "auto"),
  set: (value) => {
    if (useShared.value) multiQuality.value = value;
    else if (activeSource.value) activeSource.value.quality = value;
  },
});

const dlImage = computed({
  get: () => multiImage.value,
  set: (value) => {
    multiImage.value = value;
  },
});

const dlQualities = computed(() =>
  useShared.value
    ? allSources.value[0]?.probe.qualities ?? []
    : activeSource.value?.probe.qualities ?? []
);

// 多个来源时，共享档位默认取第一个来源的推荐值
watch(allSources, () => {
  const first = allSources.value[0];
  if (!first) return;
  multiQuality.value = pickDefaultQuality(first.probe, props.settings);
});

/** 按来源把行分组，用来在表里画出分界：哪几行属于哪个来源 */
/** 来源条数之和与实际行数之差 = 被判重掉的内容数 */
const totalItemCount = computed(() =>
  allSources.value.reduce(
    (sum, source) => sum + (source.probe.kind === "post" ? 1 : source.probe.items.length),
    0
  )
);
const dedupedCount = computed(() => totalItemCount.value - tableRows.value.length);

const tableGroups = computed(() => {
  const groups = [];
  for (const row of tableRows.value) {
    const last = groups[groups.length - 1];
    if (last && last.source === row.source) last.rows.push(row);
    else groups.push({ source: row.source, rows: [row] });
  }
  // 内容被前面的来源覆盖完的来源不再显示分组行
  return groups.filter((group) => group.rows.length > 0);
});

/** 折叠的分组（按来源输入记）：点分组行收起/展开该来源的行 */
const collapsed = ref(new Set());

function isCollapsed(input) {
  return collapsed.value.has(input);
}

/** 分组勾选：全选只覆盖这一组，不影响别的来源 */
function groupChecked(group) {
  return group.rows.length > 0 && group.rows.every((row) => isSelected(row));
}

function groupPartial(group) {
  const picked = group.rows.filter((row) => isSelected(row)).length;
  return picked > 0 && picked < group.rows.length;
}

function toggleGroupSelect(group, checked) {
  const next = new Set(selected.value);
  for (const row of group.rows) {
    if (checked) next.add(row.key);
    else next.delete(row.key);
  }
  selected.value = next;
}

function toggleGroup(input) {
  const next = new Set(collapsed.value);
  if (next.has(input)) next.delete(input);
  else next.add(input);
  collapsed.value = next;
  // 展开时该组行级联亮起（收官动效；折叠瞬时收起）
  if (!collapsed.value.has(input) && !matchMedia("(prefers-reduced-motion: reduce)").matches) {
    nextTick(() =>
      requestAnimationFrame(() => {
        const rows = [...document.querySelectorAll(`tr[data-group="${CSS.escape(input)}"]`)].slice(0, 14);
        if (rows.length) animate(rows, { opacity: [0, 1], translateY: [6, 0], duration: 300, delay: stagger(22), ease: "outQuad" });
      }),
    );
  }
}

/** 只有一个来源时不必分组（表头已经写了它是谁） */
const showGroups = computed(() => allSources.value.length > 1);

/** 有内容可列就出表（单条微博也是表里的一行） */
const hasTable = computed(() => tableRows.value.length > 0);

function openSelect(input) {
  batchInput.value = input;
  view.value = "select";
}

/** 条目在来源内的唯一标识：微博用 bid */
function entryKey(probe, entry) {
  return `${probe.kind}:${entry.bid}`;
}

const useShared = computed(() => allSources.value.length > 1);

/**
 * 所有来源都摊成同一张表：用户主页出它的每条微博，单条微博出它自己这一行。
 * 多个来源混着贴也是同一张表，不再按来源切来切去。
 *
 * `seq` 是它在表里的序号（给人看），`abs` 是它在来源里的真实序号（给序号列与命名模板的
 * {index} 用）。微博时间线从最新往最旧翻页，编号取"最新 = 1、越旧越大"——
 * 列表只会在末尾追加更旧的内容，已加载条目的编号冻结、不会漂移；
 * 预览与下载用同一个值，不会一个号一个样。
 */
const tableRows = computed(() => {
  const rows = [];
  // 内容级判重：同一条微博可能被多个来源覆盖（主页与它的单条链接），只保留先出现的
  const seenItems = new Set();
  for (const source of allSources.value) {
    if (source.probe.kind === "post") {
      const ck = `item:${source.probe.bid}`;
      if (seenItems.has(ck)) continue;
      seenItems.add(ck);
      rows.push({
        key: source.input,
        seq: rows.length + 1,
        abs: 1,
        title: source.probe.title,
        owner: source.probe.author,
        entry: source.probe.items?.[0] ?? null,
        source,
      });
      continue;
    }
    source.probe.items.forEach((entry, position) => {
      const ck = `item:${entry.bid}`;
      if (seenItems.has(ck)) return;
      seenItems.add(ck);
      rows.push({
        key: entryKey(source.probe, entry),
        seq: rows.length + 1,
        // 序号由旧到新递增（最旧 = 1）：时间线按最新在前加载，已知总数时
        // 用 总数-位置 换算（稳定不漂移）；拿不到总数时退回加载序
        abs: source.probe.total > 0
          ? Math.max(source.probe.total - position, 1)
          : position + 1,
        title: entry.title,
        owner: entry.author || source.probe.author,
        entry,
        source,
      });
    });
  }
  return rows;
});

/** 行悬停时显示该行将落盘的文件名：由后端用与下载同一个渲染器算出 */
const fileNames = ref([]);
let nameSeq = 0;

function rowExt(row) {
  if (row.entry?.has_video) return "mp4";
  if ((row.entry?.pics ?? 0) > 0) return "jpg";
  return "txt";
}

async function refreshNames() {
  const rows = tableRows.value;
  const seq = ++nameSeq;
  if (!rows.length) {
    fileNames.value = [];
    return;
  }
  // 分组取每种类型的代表行即可（同名序号不同）；mock 下逐行取也很快
  try {
    const names = await Promise.all(
      rows.map((row) =>
        api.previewNaming(
          props.settings?.naming_template || "{title}.{ext}",
          {
            date: undefined,
            ext: rowExt(row),
            sample: {
              title: row.title,
              author: row.owner,
              bid: row.entry?.bid ?? row.source.probe.bid ?? "",
              index: row.abs,
              source_kind: KIND_LABELS[row.source.probe.kind] ?? "微博",
            },
          }
        )
      )
    );
    if (seq === nameSeq) fileNames.value = names;
  } catch {
    if (seq === nameSeq) fileNames.value = [];
  }
}

watch(
  () => [tableRows.value.length, props.settings?.naming_template],
  refreshNames,
  { immediate: true }
);

async function loadMore() {
  const source = activeSource.value;
  if (!source || loadingMore.value || source.probe.exhausted) return;
  loadingMore.value = true;
  try {
    const more = await api.probeMore(source.input, batchSize.value);
    source.probe.items.push(...more.items);
    source.probe.loaded = more.loaded;
    // 微博接口不给总数：只有后端返回了非 0 的总数才覆盖（0 = 沿用用户主页的 statuses_count）
    if (more.total > 0) source.probe.total = more.total;
    source.probe.exhausted = more.exhausted;
    source.probe.note = more.note;
  } catch (error) {
    emit("toast", String(error));
  } finally {
    loadingMore.value = false;
  }
}

/** 把给定行逐个入队 */
async function startRows(rows) {
  if (!rows.length) return 0;
  let started = 0;
  for (const row of rows) {
    try {
      const probe = row.source.probe;
      const single = probe.kind === "post";
      await api.startDownload({
        bid: row.entry?.bid ?? probe.bid,
        mid: row.entry?.mid ?? probe.mid,
        source: probe.kind,
        title: row.title,
        author: row.owner || probe.author,
        uid: probe.uid,
        quality: useShared.value ? multiQuality.value : row.source.quality ?? "auto",
        image: multiImage.value,
        audio_quality: props.settings?.audio_quality || "best",
        is_audio: !!row.entry?.is_audio,
        media_url: row.entry?.media_url || "",
        page_url: probe.kind === "tvshow" ? row.source.input : "",
        image_count: single ? (row.entry?.pics ?? 0) : row.entry?.pics ?? 0,
        video_count: row.entry?.has_video ? 1 : 0,
        quality_label: row.entry
          ? row.entry.is_audio
            ? "音频"
            : row.entry.has_video
            ? "视频"
            : row.entry.pics > 0
              ? `${row.entry.pics} 图`
              : "文字"
          : "微博",
        naming: single ? singleNaming(probe) : batchNaming(probe, row.entry, row.abs),
      });
      started += 1;
    } catch (error) {
      emit("toast", `${row.title}: ${error}`);
    }
  }
  if (started) {
    emit("toast", `已加入 ${started} 个下载任务`);
    emit("goto", "transfer");
  }
  return started;
}

/** 下载：清单里勾选的行；只有一条微博时直接下这一条 */
async function downloadSelected() {
  if (!hasTable.value) return;
  const picked = tableRows.value.filter((row) => isSelected(row));
  if (!picked.length) {
    emit("toast", "先勾选要下载的内容");
    return;
  }
  await startRows(picked);
}

/** 下载全部：不看勾选，把已加载的行全部入队（没拉完会提示先解析完） */
async function downloadAll() {
  if (!hasTable.value) return;
  if (activeIsBatch.value && !activeSource.value.probe.exhausted) {
    emit("toast", "还有未加载的内容，先点「解析全部」再下载全部");
    return;
  }
  await startRows([...tableRows.value]);
}

// 重命名已下载：两段式（防误触），只对批量来源显示
const renameArmed = ref(false);
const renaming = ref(false);
const renameHint = ref("按当前命名规则改已下载条目的编号与标题（不动目录层级）");

async function renameClicked() {
  const source = activeSource.value;
  if (!source || !activeIsBatch.value) return;
  if (renaming.value) return;
  if (!renameArmed.value) {
    renaming.value = true;
    try {
      const plan = await api.renameDownloaded(source.input, source.probe.total || 0, true);
      renameArmed.value = true;
      if (plan.renamed) {
        emit("toast", `预演完成：可改名 ${plan.renamed} 项，跳过 ${plan.skipped} 项，未找到 ${plan.missing} 项；再点一次「确认改名」执行`);
      } else {
        emit("toast", "预演完成：没有找到可改名的条目");
        renameArmed.value = false;
      }
    } catch (error) {
      emit("toast", String(error));
    } finally {
      renaming.value = false;
    }
  } else {
    renaming.value = true;
    try {
      const plan = await api.renameDownloaded(source.input, source.probe.total || 0, false);
      emit("toast", `已重命名 ${plan.renamed} 项（跳过 ${plan.skipped}，未找到 ${plan.missing}）`);
    } catch (error) {
      emit("toast", String(error));
    } finally {
      renameArmed.value = false;
      renaming.value = false;
    }
  }
}

// 「下载设置」弹层（清晰度/图片规格）
const pickingDl = ref(false);
const dlPanel = ref(null);

// 工具条上的窄版「每批 N」：自定义菜单，原生 select 的去不掉的箭头会占宽
const BATCH_SIZES = [20, 50, 100, 200];
const pickingBatch = ref(false);
const batchPanel = ref(null);

// 「继续解析」下拉：拉一批之外的两种批量做法
const pickingParse = ref(false);
const parsePanel = ref(null);
const parsingAll = ref(false);

/** 一直往后拉，直到来源到底；每拉完一页刷新计数 */
async function loadAll() {
  const source = activeSource.value;
  if (!source || parsingAll.value) return;
  parsingAll.value = true;
  try {
    for (let guard = 0; guard < 200 && !source.probe.exhausted; guard += 1) {
      const before = source.probe.loaded;
      await loadMore();
      // 没有进展（出错、被限流）就停，别在原地打转
      if (source.probe.loaded === before) break;
    }
  } finally {
    parsingAll.value = false;
  }
}

async function parseAll() {
  pickingParse.value = false;
  await loadAll();
}

/** 后台解析全部并下载：把余下的分页拉完，再把全部条目排进下载队列 */
async function parseAllAndDownload() {
  pickingParse.value = false;
  await loadAll();
  await downloadAll();
}

function chooseBatchSize(size) {
  batchSize.value = size;
  pickingBatch.value = false;
}

function onDlDocumentDown(event) {
  if (pickingDl.value && dlPanel.value && !dlPanel.value.contains(event.target)) {
    pickingDl.value = false;
  }
  if (pickingParse.value && parsePanel.value && !parsePanel.value.contains(event.target)) {
    pickingParse.value = false;
  }
  if (pickingBatch.value && batchPanel.value && !batchPanel.value.contains(event.target)) {
    pickingBatch.value = false;
  }
}

function onDlKeydown(event) {
  if (event.key === "Escape") {
    pickingDl.value = false;
    pickingBatch.value = false;
    pickingParse.value = false;
  }
}

onMounted(() => {
  document.addEventListener("mousedown", onDlDocumentDown);
  document.addEventListener("keydown", onDlKeydown);
});

onUnmounted(() => {
  document.removeEventListener("mousedown", onDlDocumentDown);
  document.removeEventListener("keydown", onDlKeydown);
});

/** 步骤条点击跳转 */
function gotoStep(target) {
  if (target === "input") {
    view.value = "input";
    return;
  }
  if (target === "select" && okItems.value.length) {
    openSelect(batchInput.value || okItems.value[0].input);
  }
}

/** 清空全部解析结果 */
function resetParsed() {
  items.value = [];
  parseSkipped.value = [];
  selected.value = new Set();
  text.value = "";
  view.value = "input";
}

function removeItem(input) {
  const index = items.value.findIndex((item) => item.input === input);
  if (index >= 0) items.value.splice(index, 1);
}

/** 来源类型的中文标签（复用 download-request 那份） */
function kindLabel(kind) {
  return KIND_LABELS[kind] || kind;
}
</script>

<template>
  <div class="parse-page" :class="{ 'fill-height': isSelectView }">
    <StepHeader :steps="steps" @select="gotoStep" />

    <!-- 选择内容：解析后的独立一页 -->
    <section v-if="view === 'select' && activeSource" class="card select-page">
      <header class="select-bar">
        <button class="back" title="返回解析" @click="view = 'input'">
          <Icon name="chevronLeft" />
        </button>
        <h2 class="select-title" :title="headerTitle">{{ headerTitle }}</h2>
        <span v-if="headerTag" class="kind-tag">{{ headerTag }}</span>

        <!-- 计数与动作成组右对齐：窄窗口整体换行后仍然贴右（标签则留在左） -->
        <div class="bar-right">
        <template v-if="useShared">
          <span class="loaded-hint num">共 {{ loadedCount }} 条</span>
        </template>
        <template v-else-if="activeIsBatch">
          <span class="loaded-hint num">{{ loadedHint }}</span>
          <!-- 拉到底就不再给加载控件：只留计数与下载动作 -->
          <template v-if="!activeSource.probe.exhausted">
            <div ref="batchPanel" class="batch-picker">
              <button
                class="ghost compact"
                :class="{ on: pickingBatch }"
                aria-haspopup="menu"
                @click="pickingBatch = !pickingBatch"
              >
                每批 {{ batchSize }}
                <Icon name="chevronDown" class="caret" />
              </button>
              <Transition name="picker">
                <div v-if="pickingBatch" class="batch-pop pop-in">
                  <button
                    v-for="n in BATCH_SIZES"
                    :key="n"
                    class="batch-item"
                    :class="{ on: n === batchSize }"
                    @click="chooseBatchSize(n)"
                  >
                    每批 {{ n }}
                  </button>
                </div>
              </Transition>
            </div>

            <!-- 一个按钮：点文字拉一批，点右侧箭头选解析方式（箭头在按钮内） -->
            <div ref="parsePanel" class="parse-split" :class="{ on: pickingParse }">
              <button class="seg main" :disabled="loadingMore" @click="loadMore">
                {{ loadingMore ? `解析中 ${loadedCount}` : "继续解析" }}
              </button>
              <button
                class="seg arrow"
                :disabled="loadingMore"
                aria-haspopup="menu"
                title="更多解析方式"
                @click="pickingParse = !pickingParse"
              >
                <Icon name="chevronDown" class="caret" />
              </button>
              <Transition name="picker">
                <div v-if="pickingParse" class="parse-pop pop-in">
                  <button class="parse-item" @click="parseAll">
                    <Icon name="listDetails" />
                    解析全部
                  </button>
                  <button class="parse-item" @click="parseAllAndDownload">
                    <Icon name="cloudDownload" />
                    后台解析全部并下载
                  </button>
                </div>
              </Transition>
            </div>
          </template>
        </template>

        <!-- 重命名已下载：批量来源才显示，两段式防误触 -->
        <button
          v-if="activeIsBatch"
          class="ghost"
          :disabled="renaming"
          :title="renameHint"
          @click="renameClicked"
        >
          {{ renameArmed ? "确认改名" : "重命名已下载" }}
        </button>
        <!-- 拉完才给"下载全部"：否则点下去只下载了已加载的那一部分 -->
        <button
          v-if="hasTable && (!activeIsBatch || activeSource.probe.exhausted)"
          class="ghost"
          @click="downloadAll"
        >
          下载全部
        </button>

        <!-- 清晰度/图片规格收进弹层，工具条只留动作 -->
        <div ref="dlPanel" class="dl-settings">
          <button class="ghost" :class="{ on: pickingDl }" @click="pickingDl = !pickingDl">
            <Icon name="slidersH" />
            下载设置
          </button>

          <Transition name="picker">
            <div v-if="pickingDl" class="dl-pop pop-in">
              <label class="pop-field">
                <span>视频清晰度</span>
                <select v-model="dlQuality">
                  <option
                    v-for="quality in dlQualities"
                    :key="quality.value"
                    :value="quality.value"
                    :disabled="!quality.available"
                  >
                    {{ quality.label }}{{ quality.hint ? `（${quality.hint}）` : "" }}
                  </option>
                </select>
              </label>
              <label class="pop-field">
                <span>图片规格</span>
                <select v-model="dlImage">
                  <option v-for="image in IMAGE_CHOICES" :key="image.value" :value="image.value">
                    {{ image.label }}
                  </option>
                </select>
              </label>
              <p class="pop-note">
                只对本次下载生效；视频清晰度不可用时自动取最佳可用档。
              </p>
            </div>
          </Transition>
        </div>

        <button
          class="primary"
          :disabled="hasTable && !selectedCount"
          @click="downloadSelected"
        >
          {{ hasTable ? `下载所选 (${selectedCount})` : "加入下载" }}
        </button>
        </div>
      </header>

      <p v-if="activeSource.probe.note" class="note">{{ activeSource.probe.note }}</p>

      <div v-if="hasTable" class="table-scroll">
        <table class="batch-table">
          <thead>
            <tr>
              <th class="col-check">
                <input
                  type="checkbox"
                  :checked="allLoadedSelected"
                  title="全选已加载"
                  @change="toggleAllLoaded($event.target.checked)"
                />
              </th>
              <th class="col-idx">序号</th>
              <th>文案</th>
              <th class="col-type">类型</th>
              <th class="col-owner">博主</th>
              <th class="col-time">发布时间</th>
            </tr>
          </thead>
          <tbody>
            <template v-for="group in tableGroups" :key="group.source.input">
              <tr
                v-if="showGroups"
                class="group-row"
                :class="{ folded: isCollapsed(group.source.input) }"
                :title="isCollapsed(group.source.input) ? '展开这个来源' : '收起这个来源'"
                @click="toggleGroup(group.source.input)"
              >
                <td colspan="6">
                  <input
                    type="checkbox"
                    class="group-check"
                    :checked="groupChecked(group)"
                    :indeterminate.prop="groupPartial(group)"
                    :title="`只选择「${group.source.probe.title}」`"
                    @click.stop
                    @change="toggleGroupSelect(group, $event.target.checked)"
                  />
                  <Icon name="chevronRight" class="fold-arrow" />
                  <span class="group-kind">{{ kindLabel(group.source.probe.kind) }}</span>
                  <span class="group-title" :title="group.source.probe.title">
                    {{ group.source.probe.title }}
                  </span>
                  <span class="num faint">{{ group.rows.length }} 条</span>
                </td>
              </tr>
              <tr
                v-for="row in group.rows"
                v-show="!isCollapsed(group.source.input)"
                :key="row.key"
                :data-group="group.source.input"
                :class="{ on: isSelected(row) }"
                :title="fileNameOf(row)"
              >
                <td class="col-check">
                  <input type="checkbox" :checked="isSelected(row)" @change="toggleEntry(row)" />
                </td>
                <td class="col-idx num">{{ paddedSeq(row) }}</td>
                <td class="col-title">{{ row.title }}</td>
                <td class="col-type">{{ typeLabel(row.entry ?? {}) }}</td>
                <td class="col-owner" :title="row.owner">{{ row.owner || "—" }}</td>
                <td class="col-time num" :title="formatTime(row.entry?.created_at)">
                  {{ formatTime(row.entry?.created_at) }}
                </td>
              </tr>
            </template>
          </tbody>
        </table>
      </div>

      <!-- 底部统计只对批量清单有意义，单条微博不渲染这条空栏 -->
      <footer v-if="hasTable" class="select-foot">
        <span class="foot-count">已选 <b class="num">{{ selectedCount }}</b> 项</span>
        <span class="foot-count">共 <b class="num">{{ loadedCount }}</b> 项</span>
        <span v-if="dedupedCount > 0" class="foot-count faint">
          （已去重 <b class="num">{{ dedupedCount }}</b> 条）
        </span>
        <span class="spacer"></span>
        <span class="foot-hint faint">序号由旧到新递增（最旧为 1），加载更多不会改变已有序号</span>
        <button class="mini" @click="toggleAllLoaded(true)">全选已加载</button>
      </footer>
    </section>

    <template v-else>

    <section class="card">
      <h1>解析链接</h1>
      <p class="lead">粘贴一条或多条微博来源（单条直接回车也可），解析后再选择要下载的内容。</p>

      <textarea
        v-model="text"
        rows="7"
        spellcheck="false"
        placeholder="https://weibo.com/1234567/NbXxKq1aB&#10;https://m.weibo.cn/status/NbXxKq1aB&#10;https://weibo.com/n/博主昵称&#10;https://weibo.com/u/1234567&#10;1234567"
        @keydown.ctrl.enter="parse"
        @keydown.meta.enter="parse"
      ></textarea>

      <p class="hint">
        每行一个来源；支持单条微博、博主主页/UID、weibo.com/n/昵称 与 t.cn 短链，
        主页按时间线分页加载，可继续解析翻页。
      </p>

      <div class="actions">
        <button class="ghost" @click="pasteFromClipboard">
          <Icon name="clipboard" />
          粘贴链接
        </button>
        <span class="spacer"></span>
        <span class="kbd">Ctrl + Enter</span>
        <span class="magnet" ref="magnetWrap" @pointermove="magnetMove" @pointerleave="magnetLeave">
          <button class="primary" :data-parsing="parsing ? '' : undefined" :disabled="parsing || !text.trim()" @click="parse">
            {{ parsing ? `解析中 ${done}/${total}` : "开始解析" }}
          </button>
        </span>
      </div>

      <div class="sources">
        <span class="sources-label">支持来源</span>
        <span v-for="source in sources" :key="source.label" class="source">
          <Icon :name="source.icon" />
          {{ source.label }}
        </span>
      </div>
    </section>

    <!-- 操作说明：折叠卡片，展开后按「上手 → 解析 → 任务 → 设置 → 常见问题」组织 -->
    <section class="card help">
      <button class="help-head" :aria-expanded="showHelp" @click="showHelp = !showHelp">
        <Icon name="fileText" class="help-icon" />
        <span class="help-title">操作说明</span>
        <span class="help-sub">三步上手 · 链接格式 · 任务管理 · 常见问题</span>
        <span class="spacer"></span>
        <Icon name="chevronDown" class="help-arrow" :class="{ open: showHelp }" />
      </button>

      <!-- 折叠体：grid 行高 0fr↔1fr 过渡，内容常驻，展开/收起是平滑的高度动画 -->
      <div class="help-fold" :class="{ open: showHelp }">
        <div class="help-fold-inner">
          <div class="help-body">
            <div class="help-sec">
              <h3>快速上手</h3>
              <ol class="help-steps">
                <li>
                  <b>登录（可选）</b>：点右上角头像，扫码或网页登录。
                  不登录也能下载公开内容；登录后可下载自己的收藏、关注列表，并解锁更全的视频清晰度档位。
                </li>
                <li>
                  <b>解析</b>：把微博链接粘进上面的输入框（每行一个，可混合多行），
                  点「开始解析」或按 <span class="kbd">Ctrl + Enter</span>。
                </li>
                <li>
                  <b>下载</b>：解析完成后在「选择内容」表里勾选想要的条目，
                  点「下载所选」；想全要就直接点「下载全部」。
                </li>
              </ol>
            </div>

            <div class="help-sec">
              <h3>支持的链接</h3>
              <table class="help-table">
                <tbody>
                  <tr><td>单条微博</td><td class="num">weibo.com/1234567/NbXxKq1aB · m.weibo.cn/status/NbXxKq1aB</td></tr>
                  <tr><td>博主主页 / UID</td><td class="num">weibo.com/u/1234567 · 直接输入数字 1234567</td></tr>
                  <tr><td>昵称链接</td><td class="num">weibo.com/n/博主昵称（自动解析）</td></tr>
                  <tr><td>视频落地页</td><td class="num">weibo.com/tv/show/…（含播客音频）</td></tr>
                  <tr><td>短链</td><td class="num">t.cn/…（自动跟随跳转）</td></tr>
                </tbody>
              </table>
            </div>

            <div class="help-sec">
              <h3>解析与选择</h3>
              <ul class="help-list">
                <li>多行来源自动去重；重复内容会合并，不会重复下载。</li>
                <li>博主主页首次只加载第一页，「继续解析」往后翻页，「解析全部」一次拉完（条数多时耗时较长）。</li>
                <li>序号由旧到新（最旧为 1），文件名默认带序号，便于对照发布时间。</li>
                <li>「下载设置」里的清晰度与图片规格只对本次下载生效，默认档位在设置页修改。</li>
                <li>「重命名已下载」会按当前命名模板整理历史文件，可在弹窗里先预览再执行。</li>
              </ul>
            </div>

            <div class="help-sec">
              <h3>任务管理（传输页）</h3>
              <ul class="help-list">
                <li>每个任务都有 暂停 / 继续 / 取消；顶部有 全部暂停 / 全部开始 / 清除已结束。</li>
                <li>同时最多下载 2 个任务，其余自动排队；并发数可在「设置 · 下载」调整（1–5）。</li>
                <li>暂停、取消、退出甚至断电后进度都保存在断点文件里，继续或重新下载会自动续传。</li>
                <li>完成后「打开」用默认播放器/看图工具打开文件，「打开位置」在资源管理器中定位。</li>
              </ul>
            </div>

            <div class="help-sec">
              <h3>文件与设置</h3>
              <ul class="help-list">
                <li>保存目录与文件夹层级（默认 <span class="num">博主/年-月</span>）在「设置 · 下载」修改。</li>
                <li>文件命名模板支持魔法变量（标题、博主、序号等），在「设置 · 文件命名」里点击变量即可插入。</li>
                <li>重名处理三选一：跳过（默认）/ 覆盖 / 自动追加序号。</li>
                <li>应用更新在「设置 · 应用更新」手动检查，安装后自动重启。</li>
              </ul>
            </div>

            <div class="help-sec">
              <h3>常见问题</h3>
              <ul class="help-list">
                <li><b>显示未登录？</b>重新登录一次即可；凭据只存在本机，不会上传。</li>
                <li><b>视频不是 1080P？</b>登录后档位更全；个别帖子本身就只提供低档位或需要会员。</li>
                <li><b>下载进度不动？</b>微博接口偶发限流时任务会自动重试（默认 3 次），稍等即可。</li>
                <li><b>找不到下载的文件？</b>在传输页点「打开位置」直接定位；路径见「设置 · 下载」。</li>
                <li><b>内容库条数少？</b>微博接口只开放最近约 39 条收藏，更早的仅官方客户端可见。</li>
              </ul>
            </div>
          </div>
        </div>
      </div>
    </section>

    <!-- 解析结果入口：成功的一律进「选择内容」页，这里只留入口与失败项 -->
    <section v-if="items.length" class="card results page-in">
      <header class="results-head">
        <h2>解析结果</h2>
        <span class="count num">{{ okItems.length }}</span>
        <span class="spacer"></span>
        <button class="ghost" @click="resetParsed">清空</button>
      </header>

      <div v-if="okItems.length" class="parsed-bar">
        <button
          v-for="item in okItems"
          :key="item.input"
          class="parsed-chip"
          :title="item.probe.title || item.input"
          @click="openSelect(item.input)"
        >
          <span class="kind">{{ KIND_LABELS[item.probe.kind] }}</span>
          <span class="chip-title">{{ item.probe.title || item.input }}</span>
          <span class="num faint">{{ sourceCount(item) }}</span>
        </button>
      </div>

      <div v-if="parseSkipped.length || dedupedCount > 0" class="skipped-box">
        <p class="skipped-head">
          去重结果
          <span class="num faint">
            {{ parseSkipped.length ? `跳过 ${parseSkipped.length} 个重复来源` : "" }}
            {{ parseSkipped.length && dedupedCount ? " · " : "" }}
            {{ dedupedCount ? `合并 ${dedupedCount} 条重复内容` : "" }}
          </span>
          <span v-if="parseSkipped.length > 5" class="skipped-more faint">（列表内可滚动）</span>
        </p>
        <ul class="skipped-list">
          <li v-for="(item, index) in parseSkipped" :key="`${item.input}-${index}`">
            <span class="skipped-input" :title="item.input">{{ item.input }}</span>
            <span class="skipped-reason">{{ item.reason }}</span>
          </li>
          <li v-if="dedupedCount > 0" class="skipped-merged">
            <span class="skipped-reason">
              另有 {{ dedupedCount }} 条内容与已有来源重复，已合并（不重复下载）
            </span>
          </li>
        </ul>
      </div>

      <ul v-if="failedItems.length" class="list">
        <li v-for="item in failedItems" :key="item.input" class="item failed">
          <div class="meta">
            <p class="title">{{ item.input }}</p>
            <p class="error">{{ item.error }}</p>
          </div>
          <button class="remove" title="移除" @click="removeItem(item.input)">✕</button>
        </li>
      </ul>

      <p v-if="!login.logged_in && okItems.length" class="login-tip">
        未登录时可解析公开微博；登录后可下载自己的收藏、获取更全的视频清晰度。
      </p>
    </section>
    </template>
  </div>
</template>

<style scoped>
.card {
  padding: 20px 22px;
  background: var(--card);
  border: 1px solid var(--line);
  border-radius: var(--radius-lg);
}

h1 {
  margin: 0;
  font-size: 25px;
  font-weight: 700;
  letter-spacing: -0.2px;
}

.lead {
  margin: 7px 0 0;
  font-size: 13px;
  color: var(--muted);
}

/* 勾选框不算"输入框"：这条规则里的 width:100% 会把它拉成整行宽 */
textarea,
input:not([type="checkbox"]) {
  width: 100%;
  padding: 12px 14px;
  color: var(--text);
  background: var(--field);
  border: 1px solid var(--line);
  border-radius: var(--radius);
  resize: vertical;
  line-height: 1.7;
  transition: border-color 0.15s ease;
}

textarea::placeholder,
input::placeholder {
  color: var(--faint);
}

textarea:focus,
input:focus {
  outline: none;
  border-color: var(--accent-line);
  box-shadow: 0 0 0 3px var(--accent-soft);
}

.hint {
  margin: 9px 0 0;
  font-size: 12px;
  color: var(--faint);
}

.magnet {
  display: inline-flex;
  transition: transform var(--motion) var(--ease-out);
}

.actions {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-top: 16px;
}

.spacer {
  flex: 1;
}

.kbd {
  font-size: 11.5px;
  color: var(--faint);
}

.primary {
  padding: 9px 22px;
  font-weight: 600;
  color: #fff;
  background: var(--accent);
  border-radius: var(--radius-sm);
  transition: background 0.15s ease;
}

.primary:hover:not(:disabled) {
  background: var(--accent-dark);
}

.primary:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.ghost {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  padding: 8px 14px;
  font-size: 12.5px;
  color: var(--text);
  background: var(--field);
  border: 1px solid var(--line);
  border-radius: var(--radius-sm);
}

.ghost:hover {
  border-color: var(--accent-line);
  background: var(--raised);
}

.ghost svg {
  width: 18px;
  height: 18px;
  color: var(--muted);
}

.sources {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px 16px;
  margin-top: 14px;
  padding-top: 13px;
  border-top: 1px solid var(--line-soft);
  font-size: 12px;
  color: var(--muted);
}

.sources-label {
  font-weight: 600;
  color: var(--text);
}

.source {
  display: inline-flex;
  align-items: center;
  gap: 5px;
}

.source svg {
  flex: none;
  width: 17px;
  height: 17px;
  color: var(--accent);
}

/* 解析结果里的去重明细 */
.skipped-box {
  margin-top: 12px;
  padding: 10px 12px;
  background: var(--raised);
  border: 1px solid var(--line-soft);
  border-radius: var(--radius-sm);
}

.skipped-head {
  display: flex;
  align-items: baseline;
  gap: 8px;
  margin: 0;
  font-size: 12.5px;
  font-weight: 600;
}

/* 明细不撑高整页：超过几行就在框内滚，整页该滚动的距离不受它影响 */
.skipped-list {
  margin: 7px 0 0;
  padding: 0;
  list-style: none;
  max-height: 108px;
  overflow-y: auto;
}

.skipped-list li {
  display: flex;
  align-items: baseline;
  gap: 8px;
  padding: 3px 0;
  font-size: 12px;
  color: var(--muted);
}

.skipped-input {
  flex: 0 1 auto;
  min-width: 0;
  max-width: 58%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-family: ui-monospace, "Cascadia Mono", Consolas, monospace;
  color: var(--text);
}

.skipped-reason {
  flex: 1;
  min-width: 0;
  color: var(--faint);
  overflow-wrap: anywhere;
}

/* 输入页里的"已解析来源"入口：固定两列网格，一排两个、宽窄统一 */
.parsed-bar {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 8px;
  margin-top: 0;
}

/* 窄窗口放不下两个时就退成单列（标题省略号兜底） */
@media (max-width: 860px) {
  .parsed-bar {
    grid-template-columns: minmax(0, 1fr);
  }
}

.parsed-chip {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
  padding: 7px 12px;
  font-size: 12.5px;
  color: var(--text);
  background: var(--field);
  border: 1px solid var(--line);
  border-radius: var(--radius-sm);
}

.parsed-chip:hover {
  border-color: var(--accent-line);
  background: var(--raised);
}

/* 类型标签与条数不参与收缩，标题占满剩余宽度后省略 */
.parsed-chip .kind,
.parsed-chip .num {
  flex: none;
}

.parsed-chip .chip-title {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* 「下载设置」弹层：清晰度/图片规格，工具条只留动作按钮 */
.dl-settings {
  position: relative;
  flex: none;
}

.dl-settings .ghost {
  gap: 7px;
  padding: 6px 11px;
  font-size: 12.5px;
}

.dl-settings .ghost svg {
  width: 18px;
  height: 18px;
}

.dl-settings .ghost.on {
  color: var(--accent);
  border-color: var(--accent-line);
  background: var(--raised);
}

.dl-pop {
  position: absolute;
  top: calc(100% + 6px);
  right: 0;
  z-index: 50;
  width: min(300px, calc(100vw - 60px));
  padding: 12px;
  text-align: left;
  background: var(--card);
  border: 1px solid var(--line);
  border-radius: var(--radius);
  box-shadow: 0 16px 44px rgba(0, 0, 0, 0.5);
}

.pop-field {
  display: flex;
  flex-direction: column;
  gap: 5px;
}

.pop-field + .pop-field {
  margin-top: 10px;
}

.pop-field span {
  font-size: 11.5px;
  color: var(--faint);
}

.pop-field select {
  width: 100%;
  height: 30px;
  padding: 0 9px;
  font-size: 12.5px;
  color: var(--text);
  background: var(--field);
  border: 1px solid var(--line);
  border-radius: var(--radius-sm);
}

.pop-note {
  margin: 10px 0 0;
  font-size: 11px;
  line-height: 1.5;
  color: var(--faint);
}

/* 选择页占满可用高度：顶部工具条与底部统计不随滚动移动 */
.fill-height {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
}

.fill-height .select-page {
  flex: 1;
  min-height: 0;
}

/* 输入页：卡片垂直排列，高度都按内容走——窗口有富余时下方留底色，
   不把卡片硬撑成空壳（说明卡与结果卡都遵循这条） */
.parse-page:not(.fill-height) {
  display: flex;
  flex-direction: column;
  min-height: 100%;
}

/* 解析链接卡：高度按内容，**不参与拉伸**。 */
.parse-page:not(.fill-height) > .card {
  display: flex;
  flex: 0 1 auto;
  flex-direction: column;
  min-height: 300px;
}

.parse-page:not(.fill-height) > .card textarea {
  flex: 1 1 auto;
  min-height: 110px;
  height: 110px;
}

/* 解析结果卡：高度按内容——空状态不再被拉成大空壳；
   明细列表自带 108px 上限并在框内滚，条数多也不会撑高整页 */
.parse-page:not(.fill-height) > .results {
  flex: 0 0 auto;
  min-height: 0;
}

/* 选择内容页：解析结果独立成一页，表格 + 分批加载 */
.select-page {
  display: flex;
  flex-direction: column;
  padding: 0;
  overflow: hidden;
}

.select-bar .ghost,
.select-bar .primary {
  flex: none;
  padding: 6px 13px;
  font-size: 12.5px;
  border-radius: var(--radius-sm);
}

.select-bar {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px 10px;
  padding: 10px 14px;
  border-bottom: 1px solid var(--line-soft);
  /* 独立层叠上下文：弹层（每批/解析方式/下载设置）必须整体压过后方的表格，
     否则深色主题下弹层与表格行几乎同色，会被当成"透明/错乱"（实测踩过） */
  position: relative;
  z-index: 30;
}

/* 计数与动作成组：整组靠右（margin-left:auto），窄到放不下时整组换行 */
.bar-right {
  display: flex;
  flex-wrap: wrap;
  justify-content: flex-end;
  align-items: center;
  gap: 8px;
  margin-left: auto;
}

/* 窄版"每批 N"：不带下拉箭头（原生 select 的箭头去不掉，所以自绘菜单） */
.batch-picker {
  position: relative;
  flex: none;
}

.ghost.compact {
  padding: 6px 10px;
  font-size: 12.5px;
}

.ghost.compact.on {
  color: var(--accent);
  border-color: var(--accent-line);
  background: var(--raised);
}

.batch-pop {
  position: absolute;
  top: calc(100% + 6px);
  left: 0;
  z-index: 50;
  min-width: 108px;
  padding: 4px;
  background: var(--card);
  border: 1px solid var(--line);
  border-radius: var(--radius);
  box-shadow: 0 16px 44px rgba(0, 0, 0, 0.5);
}

.batch-item {
  display: block;
  width: 100%;
  padding: 6px 9px;
  font-size: 12.5px;
  color: var(--text);
  text-align: left;
  border-radius: var(--radius-sm);
}

.batch-item:hover {
  background: var(--hover);
}

.batch-item.on {
  color: var(--accent);
}

.back {
  display: grid;
  flex: none;
  place-items: center;
  width: 28px;
  height: 28px;
  color: var(--muted);
  border-radius: var(--radius-sm);
}

.back:hover {
  color: var(--text);
  background: var(--hover);
}

.back svg {
  width: 20px;
  height: 20px;
}

/* 标题按内容占宽，一行布局下标题可伸缩：超出部分省略号（悬停看全名） */
.select-title {
  flex: 0 1 auto;
  max-width: calc(100% - 460px);
  min-width: 28px;
  margin: 0;
  font-size: 14px;
  font-weight: 700;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.kind-tag {
  flex: none;
  /* 和标题靠拢：整行 gap 是 10px，这里收掉 4px */
  margin-left: -4px;
  padding: 2px 8px;
  font-size: 11px;
  color: var(--accent);
  background: var(--accent-soft);
  border: 1px solid var(--accent-line);
  border-radius: 999px;
}

.loaded-hint {
  font-size: 12px;
  color: var(--muted);
  white-space: nowrap;
}

.table-scroll {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
}

.batch-table {
  width: 100%;
  border-collapse: collapse;
  table-layout: fixed;
  font-size: 12.5px;
}

.batch-table th {
  position: sticky;
  top: 0;
  z-index: 1;
  padding: 9px 10px;
  font-weight: 600;
  color: var(--muted);
  text-align: left;
  background: var(--card);
  border-bottom: 1px solid var(--line);
}

/* 分组行：多来源时用它把各组分开，一眼看出哪几行属于哪个来源 */
.batch-table tr.group-row {
  cursor: pointer;
  user-select: none;
}

/* 分组选择框：和行内的勾选框左对齐，别被折叠点击抢走事件 */
.group-check {
  margin-right: 8px;
  vertical-align: -3px;
}

.fold-arrow {
  width: 15px;
  height: 15px;
  margin-right: 4px;
  color: var(--faint);
  vertical-align: -2px;
  transform: rotate(90deg);
  transition: transform 0.15s ease;
}

.batch-table tr.group-row.folded .fold-arrow {
  transform: rotate(0deg);
}

.batch-table tr.group-row.folded .group-title {
  color: var(--muted);
}

.batch-table tr.group-row:hover .fold-arrow {
  color: var(--accent);
}

.batch-table tr.group-row td {
  padding: 7px 10px 6px;
  background: var(--raised);
  border-top: 1px solid var(--line);
  border-bottom: 1px solid var(--line-soft);
}

.batch-table tbody tr.group-row:first-child td {
  border-top: none;
}

.batch-table tr.group-row:hover td {
  background: var(--raised);
}

.group-kind {
  margin-right: 8px;
  padding: 1px 7px;
  font-size: 11px;
  color: var(--accent);
  background: var(--accent-soft);
  border: 1px solid var(--accent-line);
  border-radius: 999px;
}

.group-title {
  margin-right: 8px;
  font-size: 12.5px;
  font-weight: 600;
  color: var(--text);
}

.batch-table td {
  padding: 8px 10px;
  border-bottom: 1px solid var(--line-soft);
}

.batch-table tbody tr:hover td {
  background: var(--raised);
}

.batch-table tr.on td {
  background: var(--accent-soft);
}

.col-check {
  width: 36px;
}

.col-idx {
  width: 58px;
  color: var(--faint);
}

.col-type {
  width: 64px;
  text-align: center;
  color: var(--muted);
}

/* 表头文字对齐要压过 .batch-table th 的 left */
.batch-table th.col-owner,
.batch-table td.col-owner {
  width: 130px;
  text-align: center;
}

.batch-table th.col-time,
.batch-table td.col-time {
  width: 96px;
  text-align: center;
  color: var(--muted);
}

.col-title {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.select-foot {
  display: flex;
  align-items: center;
  gap: 18px;
  padding: 11px 16px;
  border-top: 1px solid var(--line-soft);
}

.foot-count {
  font-size: 12.5px;
  color: var(--muted);
}

.foot-count b {
  color: var(--text);
  font-weight: 600;
}

.foot-hint {
  font-size: 11.5px;
}

/* 解析结果 */
.results {
  margin-top: 16px;
}

.results-head {
  display: flex;
  align-items: center;
  gap: 9px;
  margin-bottom: 8px;
}

h2 {
  margin: 0;
  font-size: 15px;
  font-weight: 700;
}

.count {
  font-size: 12px;
  color: var(--faint);
}

.list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.item {
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 12px 14px;
  background: var(--raised);
  border: 1px solid var(--line-soft);
  border-radius: var(--radius);
}

.item.failed {
  background: var(--fail-bg);
  border-color: var(--fail-line);
}

.meta {
  flex: 1;
  min-width: 0;
}

.title {
  margin: 0;
  font-size: 13.5px;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* 勾选状态变化：整行底色过渡已有；序号与标题列给一次轻弹 */
.batch-table tbody tr.on td:first-child {
  animation: cell-pop 240ms var(--ease-out);
}

@keyframes cell-pop {
  0% { transform: scale(0.96); }
  60% { transform: scale(1.03); }
  100% { transform: none; }
}

.note {
  display: flex;
  align-items: baseline;
  gap: 6px;
  margin: 10px 0 2px;
  padding: 8px 12px;
  font-size: 12px;
  line-height: 1.55;
  color: var(--warn);
  background: color-mix(in srgb, var(--warn) 9%, transparent);
  border: 1px solid color-mix(in srgb, var(--warn) 26%, transparent);
  border-radius: var(--radius-sm);
}

.note::before {
  content: "!";
  flex: none;
  width: 15px;
  height: 15px;
  align-self: center;
  display: grid;
  place-items: center;
  font-size: 10.5px;
  font-weight: 700;
  color: var(--warn);
  border: 1px solid color-mix(in srgb, var(--warn) 45%, transparent);
  border-radius: 50%;
}

.error {
  margin: 4px 0 0;
  font-size: 12px;
  color: var(--err);
}

.remove {
  flex: none;
  width: 30px;
  height: 30px;
  font-size: 12px;
  color: var(--faint);
  border-radius: var(--radius-sm);
}

.remove:hover {
  color: var(--err);
  background: var(--fail-bg);
}

.login-tip {
  margin: 14px 0 0;
  padding: 9px 12px;
  font-size: 12px;
  color: var(--accent-dark);
  background: var(--accent-soft);
  border-radius: var(--radius-sm);
}

.mini {
  padding: 3px 10px;
  font-size: 11.5px;
  color: var(--muted);
  background: var(--field);
  border: 1px solid var(--line);
  border-radius: var(--radius-sm);
}

.mini:hover {
  color: var(--text);
  border-color: var(--accent-line);
}

/* 「每批 N」上的下拉箭头，以及「继续解析 + 解析方式」按钮 */
.batch-picker .ghost.compact {
  display: inline-flex;
  align-items: center;
  gap: 4px;
}

/* 下拉箭头：菜单打开时转 180°（过渡与旋转复合在同一属性上） */
.caret {
  flex: none;
  width: 17px;
  height: 17px;
  transition: transform var(--motion-fast) var(--ease-out);
}

.ghost.compact.on .caret,
.parse-split.on .caret {
  transform: rotate(180deg);
}

/* 边框画在容器上、两个按钮透明无边框：箭头因此看起来是在按钮里面 */
.parse-split {
  position: relative;
  display: inline-flex;
  align-items: stretch;
  flex: none;
  color: var(--text);
  background: var(--field);
  border: 1px solid var(--line);
  border-radius: var(--radius-sm);
}

.parse-split .seg {
  display: inline-flex;
  align-items: center;
  padding: 6px 4px 6px 11px;
  font-size: 12.5px;
  color: inherit;
  background: none;
  border: 0;
  border-radius: 0;
}

.parse-split .seg:first-of-type {
  border-top-left-radius: var(--radius-sm);
  border-bottom-left-radius: var(--radius-sm);
}

.parse-split .seg:hover:not(:disabled) {
  color: var(--accent);
}

.parse-split .seg:disabled {
  opacity: 0.55;
}

.parse-split .seg.arrow {
  padding: 6px 9px 6px 4px;
  border-top-right-radius: var(--radius-sm);
  border-bottom-right-radius: var(--radius-sm);
}

.parse-split .seg.arrow svg {
  display: block;
  width: 17px;
  height: 17px;
  color: var(--muted);
}

.parse-split .seg.arrow:hover:not(:disabled) svg {
  color: var(--accent);
}

.parse-split.on {
  border-color: var(--accent-line);
  background: var(--raised);
}

.parse-split.on .seg,
.parse-split.on .seg.arrow svg {
  color: var(--accent);
}

.parse-pop {
  position: absolute;
  top: calc(100% + 6px);
  right: 0;
  z-index: 50;
  min-width: 186px;
  padding: 4px;
  background: var(--card);
  border: 1px solid var(--line);
  border-radius: var(--radius);
  box-shadow: 0 16px 44px rgba(0, 0, 0, 0.5);
}

.parse-item {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 7px 9px;
  font-size: 12.5px;
  color: var(--text);
  text-align: left;
  white-space: nowrap;
  border-radius: var(--radius-sm);
}

.parse-item svg {
  flex: none;
  width: 18px;
  height: 18px;
  color: var(--muted);
}

.parse-item:hover {
  background: var(--hover);
}

.parse-item:hover svg {
  color: var(--text);
}
</style>

<style scoped>
/* 第六轮追加：来源类型徽标出现时轻弹 */
.kind-tag {
  animation: tag-pop 280ms var(--ease-out-expo);
}

@keyframes tag-pop {
  0% { transform: scale(0.85); opacity: 0; }
  60% { transform: scale(1.06); opacity: 1; }
  100% { transform: none; opacity: 1; }
}
</style>


<style scoped>
/* 第七轮追加：表格行悬停左缘指示线。两个踩过的坑：
   1) 悬停行背景设在 td 上（tr:hover td），td 的不透明背景会盖住 tr 的
      inset shadow——线画在 tr 上只能露出边角。线必须挂在
      td:first-child 上才整行可见。
   2) 不给 box-shadow 挂过渡——快速划过多行时余晖叠成一排碎粉线。 */
.batch-table tbody tr:hover td:first-child {
  box-shadow: inset 2px 0 0 var(--accent);
}
</style>

<style scoped>
/* 第七轮追加：解析进行中主按钮扫光（reduced-motion 全局关） */
.primary[data-parsing] {
  position: relative;
  overflow: hidden;
}

.primary[data-parsing]::after {
  content: "";
  position: absolute;
  inset: 0;
  background: linear-gradient(105deg, transparent 42%, rgba(255, 255, 255, 0.22) 50%, transparent 58%);
  animation: parse-shimmer 1.2s linear infinite;
}

@keyframes parse-shimmer {
  from { transform: translateX(-130%); }
  to { transform: translateX(130%); }
}

/* ── 操作说明：折叠卡片 ── */
.help {
  padding: 4px 8px;
}

/* 输入页是三卡分摊窗口高度的布局：说明卡只按内容高度走，
   不吃「> .card」的 300px 最小高度（那是留给输入卡与结果卡的），
   折叠后整卡就只剩标题一行 */
.parse-page:not(.fill-height) > .card.help {
  flex: 0 0 auto;
  min-height: 0;
}

/* 折叠体：行高 0fr↔1fr 过渡（内容常驻，展开/收起是平滑的高度动画）；
   「减少动态」下由全局规则自动降级为瞬切 */
.help-fold {
  display: grid;
  grid-template-rows: 0fr;
  transition: grid-template-rows var(--motion) var(--ease-out);
}

.help-fold.open {
  grid-template-rows: 1fr;
}

.help-fold-inner {
  min-height: 0;
  overflow: hidden;
}

.help-head {
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  padding: 12px 14px;
  text-align: left;
  color: var(--text);
  background: none;
  border: none;
  border-radius: var(--radius);
  cursor: pointer;
}

.help-head:hover {
  background: var(--hover);
}

.help-icon {
  flex: none;
  width: 19px;
  height: 19px;
  color: var(--accent);
}

.help-title {
  font-size: 14px;
  font-weight: 600;
}

.help-sub {
  font-size: 12px;
  color: var(--faint);
}

.help-arrow {
  flex: none;
  width: 16px;
  height: 16px;
  color: var(--faint);
  transition: transform var(--motion-fast) var(--ease-out);
}

.help-arrow.open {
  transform: rotate(180deg);
}

.help-body {
  padding: 2px 14px 16px;
}

.help-sec + .help-sec {
  margin-top: 16px;
  padding-top: 15px;
  border-top: 1px solid var(--line-soft);
}

.help-sec h3 {
  margin: 0 0 9px;
  font-size: 13px;
  font-weight: 600;
  color: var(--accent-dark);
}

/* 章节标题前缀小竖条：与「解析来源」步骤条的强调语言一致 */
.help-sec h3::before {
  content: "";
  display: inline-block;
  width: 3px;
  height: 12px;
  margin-right: 7px;
  vertical-align: -1.5px;
  background: var(--accent);
  border-radius: 2px;
}

.help-steps,
.help-list {
  margin: 0;
  padding-left: 19px;
  display: flex;
  flex-direction: column;
  gap: 7px;
  font-size: 12.8px;
  line-height: 1.75;
  color: var(--muted);
}

.help-steps li::marker {
  color: var(--accent);
  font-weight: 600;
}

.help-steps b,
.help-list b {
  color: var(--text);
  font-weight: 600;
}

.help-body .kbd {
  padding: 1px 6px;
  color: var(--text);
  background: var(--raised);
  border: 1px solid var(--line);
  border-radius: 5px;
}

.help-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 12.8px;
  color: var(--muted);
}

.help-table td {
  padding: 7px 10px 7px 0;
  border-top: 1px solid var(--line-soft);
  vertical-align: top;
}

.help-table tr:first-child td {
  border-top: none;
}

.help-table td:first-child {
  width: 118px;
  color: var(--text);
  font-weight: 500;
  white-space: nowrap;
}

.help-table .num {
  overflow-wrap: anywhere;
}
</style>
