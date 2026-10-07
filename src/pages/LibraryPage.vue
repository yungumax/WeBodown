<script setup>
import { computed, nextTick, onMounted, ref, watch } from "vue";
import { animate, stagger } from "animejs";
import Icon from "../components/Icon.vue";
import * as api from "../api";

const props = defineProps({
  login: { type: Object, required: true },
});
const emit = defineEmits(["goto", "open-source", "login"]);

const TAB = { fav: "fav", follow: "follow" };
const tab = ref(TAB.fav);
const loading = ref(false);
const error = ref("");
const account = ref(null);
const query = ref("");
const picked = ref(new Set());

/** 当前标签下是否全部已选（用于"全部选择/取消全选"按钮文案） */
const allPicked = computed(
  () => list.value.length > 0 && list.value.every((item) => picked.value.has(keyOf(item)))
);

/** 全选 / 取消全选：只作用于当前标签下的条目 */
function toggleAllPicked() {
  const next = new Set(picked.value);
  if (allPicked.value) {
    list.value.forEach((item) => next.delete(keyOf(item)));
  } else {
    list.value.forEach((item) => next.add(keyOf(item)));
  }
  picked.value = next;
}

const loggedIn = computed(() => !!props.login?.logged_in);

const list = computed(() => {
  const items = tab.value === TAB.fav ? account.value?.created ?? [] : account.value?.subscribed ?? [];
  const keyword = query.value.trim().toLowerCase();
  if (!keyword) return items;
  return items.filter((item) =>
    [item.title, item.owner].some((text) => (text ?? "").toLowerCase().includes(keyword))
  );
});

/** 两个标签合起来：勾选可以跨标签，取来源时不能只看当前标签那一列 */
const allItems = computed(() => [
  ...(account.value?.created ?? []),
  ...(account.value?.subscribed ?? []),
]);

/** 勾选用的键：收藏与关注是两套命名空间，可能撞号，得带上 kind */
const keyOf = (item) => `${item.kind || "fav"}:${item.id}`;

/** 条目对应的微博链接：收藏 → 单条微博；关注 → 博主主页 */
const itemUrl = (item) =>
  item.kind === "follow"
    ? `https://weibo.com/u/${item.owner_mid}`
    : `https://m.weibo.cn/status/${item.bid}`;

/** 卡片副标题与数量文案 */
function subLabel(item) {
  if (item.kind === "follow") return `关注的人 · UID ${item.owner_mid}`;
  return `收藏的微博 · ${formatTime(item.created_at)}`;
}

function countLabel(item) {
  if (item.kind === "follow") return `${item.media_count} 条微博`;
  if (item.pics > 0) return `${item.pics} 图`;
  return "文字";
}

function formatTime(unixSecs) {
  if (!unixSecs) return "—";
  const d = new Date(unixSecs * 1000);
  const pad = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

function switchTab(next) {
  if (tab.value === next) return;
  tab.value = next;
  query.value = "";
}

async function load() {
  if (!loggedIn.value || loading.value) return;
  loading.value = true;
  error.value = "";
  try {
    account.value = await api.libraryFolders();
    picked.value = new Set();
  } catch (e) {
    error.value = String(e);
    account.value = null;
  } finally {
    loading.value = false;
  }
}

function togglePick(key) {
  const next = new Set(picked.value);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  picked.value = next;
}

/** 点卡片：交给解析页按链接解析（那边继续筛、还能改清晰度） */
function openItem(item) {
  emit("open-source", itemUrl(item));
}

/** 选中若干条（可以跨标签）→ 多行一起交给解析页 */
function openPicked() {
  const selected = allItems.value.filter((item) => picked.value.has(keyOf(item)));
  if (!selected.length) return;
  emit("open-source", selected.map(itemUrl).join("\n"));
}

watch(
  () => props.login?.logged_in,
  (on) => {
    if (on) load();
    else {
      account.value = null;
    }
  },
  { immediate: true }
);

onMounted(() => {
  if (loggedIn.value) load();
});

/** 卡片 3D 微倾斜（±2.4°）：指针在哪边卡片就朝哪边轻轻转，移开复位。
    倾斜写在卡片自己的 transform 上（含悬停那 1px 上浮），纯动效零视觉。 */
let tiltedCard = null;

function onTilt(event) {
  if (matchMedia("(prefers-reduced-motion: reduce)").matches) return;
  const card = event.target instanceof Element ? event.target.closest(".collection") : null;
  if (!card) return;
  if (tiltedCard && tiltedCard !== card) tiltedCard.style.transform = "";
  tiltedCard = card;
  const r = card.getBoundingClientRect();
  const px = (event.clientX - r.left) / r.width - 0.5;
  const py = (event.clientY - r.top) / r.height - 0.5;
  card.style.transform = `translateY(-1px) rotateX(${(-py * 2.4).toFixed(2)}deg) rotateY(${(px * 2.4).toFixed(2)}deg)`;
}
function offTilt() {
  // pointerleave 绑在容器上，target 是容器不是卡——按记录复位
  if (tiltedCard) {
    tiltedCard.style.transform = "";
    tiltedCard = null;
  }
}

// 入场：卡片首次渲染出来后逐张浮起（一次性编排，减少动态下不演）
let roseIn = false;
watch(
  () => list.value.length,
  async (n) => {
    if (!n || roseIn) return;
    roseIn = true;
    await nextTick();
    if (matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    animate(".card .collection", {
      opacity: [0, 1],
      translateY: [16, 0],
      duration: 520,
      delay: stagger(60),
      ease: "outExpo",
    });
  },
);
</script>

<template>
  <div class="lib-page" :class="{ 'fill-height': loggedIn }">
    <section class="card">
      <div class="tabs">
        <button :class="{ active: tab === TAB.fav }" @click="switchTab(TAB.fav)">
          我收藏的微博
          <span v-if="account" class="badge">{{ account.created.length }}</span>
        </button>
        <button :class="{ active: tab === TAB.follow }" @click="switchTab(TAB.follow)">
          我关注的人
          <span v-if="account" class="badge">{{ account.subscribed.length }}</span>
        </button>
      </div>

      <!-- 未登录：登录后连接内容库 -->
      <div v-if="!loggedIn" class="empty tall">
        <Icon name="lock" class="empty-icon" />
        <div class="empty-text">
          <p class="title">登录后连接你的内容库</p>
          <p class="hint">
            登录凭据只存在本机（数据目录里的 cookies.json），内容库不会显示或导出 Cookie 的内容。
          </p>
        </div>
        <button class="primary" @click="emit('login')">登录账号</button>
      </div>

      <template v-else>
        <header class="head">
          <div>
            <h2>{{ tab === TAB.fav ? "我收藏的微博" : "我关注的人" }}</h2>
            <p class="meta">
              {{ tab === TAB.fav ? "收藏夹 · 点卡片去解析该条微博" : "关注列表 · 点卡片去解析 TA 的主页" }}
            </p>
            <p
              v-if="tab === TAB.fav ? account?.fav_note : account?.follow_note"
              class="meta dim"
            >
              {{ tab === TAB.fav ? account?.fav_note : account?.follow_note }}
            </p>
          </div>
          <span class="grow"></span>
          <button v-if="list.length" class="ghost" @click="toggleAllPicked">
            {{ allPicked ? "取消全选" : "全部选择" }}
          </button>
          <button class="ghost" :disabled="loading" title="刷新" @click="load">
            <Icon name="refresh" class="btn-icon" :class="{ spin: loading }" />
          </button>
        </header>

        <div class="search">
          <Icon name="search" class="search-icon" />
          <input v-model="query" spellcheck="false" placeholder="搜索文案或博主" />
        </div>

        <p v-if="loading && !account" class="hint pad">正在读取账号里的内容…</p>
        <div v-else-if="error" class="empty">
          <div class="empty-text">
            <p class="title">没读出来</p>
            <p class="hint">{{ error }}</p>
          </div>
          <button class="ghost" @click="load">重试</button>
        </div>

        <div v-else-if="list.length" class="cards page-in" @pointermove="onTilt" @pointerleave="offTilt">
          <article
            v-for="item in list"
            :key="keyOf(item)"
            class="collection"
            :class="{ on: picked.has(keyOf(item)) }"
            @click="openItem(item)"
          >
            <div class="thumb">
              <div class="thumb-count">
                <Icon :name="item.kind === 'follow' ? 'user' : item.pics > 0 ? 'photo' : 'link'" />
                <b>{{ item.kind === "follow" ? item.media_count : item.pics > 0 ? item.pics : "文" }}</b>
              </div>
            </div>
            <div class="body">
              <p class="ctitle" :title="item.title">{{ item.title }}</p>
              <p class="sub">{{ subLabel(item) }}</p>
              <p class="num">{{ countLabel(item) }}</p>
            </div>
            <button
              class="pick"
              :title="picked.has(keyOf(item)) ? '取消选择' : '选择这条'"
              @click.stop="togglePick(keyOf(item))"
            >
              <Icon :name="picked.has(keyOf(item)) ? 'check' : 'plus'" />
            </button>
          </article>
        </div>

        <div v-else class="empty">
          <div class="empty-text">
            <p class="title">
              {{ query ? "没有匹配的内容" : tab === TAB.fav ? "这个账号还没有收藏微博" : "还没有关注的人" }}
            </p>
            <p class="hint">
              {{
                query
                  ? "换个关键词试试。"
                  : tab === TAB.fav
                    ? "在微博上收藏一条微博，这里就能看到。"
                    : "在微博上关注一些人，这里就能看到。"
              }}
            </p>
          </div>
        </div>

        <footer v-if="list.length" class="foot">
          <span class="count">已选 {{ picked.size }} 条</span>
          <button class="primary" :disabled="!picked.size" @click="openPicked">解析所选来源</button>
        </footer>
      </template>
    </section>
  </div>
</template>

<style scoped>
.card {
  padding: 18px 22px 20px;
  background: var(--card);
  border: 1px solid var(--line);
  border-radius: var(--radius-lg);
}

.tabs {
  display: flex;
  gap: 18px;
  margin-bottom: 16px;
  border-bottom: 1px solid var(--line);
}

.tabs button {
  display: inline-flex;
  align-items: baseline;
  gap: 6px;
  padding: 7px 0 9px;
  margin-bottom: -1px;
  font-size: 13.5px;
  color: var(--muted);
  background: none;
  border: none;
  border-bottom: 2px solid transparent;
  cursor: pointer;
}

.tabs button.active {
  font-weight: 600;
  color: var(--accent-ink);
  border-bottom-color: var(--accent);
}

.badge {
  font-size: 11.5px;
  color: var(--faint);
}

.head {
  display: flex;
  align-items: center;
  gap: 10px;
}

.grow {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}

h2 {
  margin: 0;
  font-size: 20px;
  font-weight: 700;
  letter-spacing: -0.2px;
}

.meta {
  margin: 5px 0 0;
  font-size: 12.5px;
  color: var(--muted);
}

.meta.dim {
  margin-top: 3px;
  font-size: 11.5px;
  color: var(--faint);
}

.btn-icon {
  width: 16px;
  height: 16px;
}

.search {
  position: relative;
  margin-top: 14px;
}

.search-icon {
  position: absolute;
  top: 50%;
  left: 11px;
  width: 16px;
  height: 16px;
  color: var(--faint);
  transform: translateY(-50%);
}

.search input {
  width: 100%;
  padding: 9px 12px 9px 34px;
  font: inherit;
  font-size: 13px;
  color: var(--text);
  background: var(--field);
  border: 1px solid var(--line);
  border-radius: var(--radius);
  transition: border-color var(--motion-fast) var(--ease-out);
}

.search input:focus-visible {
  outline: none;
  border-color: var(--accent-line);
  box-shadow: 0 0 0 3px var(--accent-soft);
}

.pad {
  margin: 16px 0 0;
}

.cards {
  perspective: 640px;
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  /* 卡片之间的间隔略微放大，透气一点 */
  gap: 18px;
  margin-top: 16px;
}

.collection {
  position: relative;
  display: flex;
  gap: 13px;
  padding: 12px;
  background: var(--card);
  /* 悬停反馈：边框变色 + 1px 上浮，平面语言里的一点"可点" */
  transition:
    background var(--motion-fast) var(--ease-out),
    border-color var(--motion-fast) var(--ease-out),
    transform var(--motion-fast) var(--ease-out);
  border: 1px solid var(--line);
  border-radius: var(--radius);
  cursor: pointer;
}

.collection:hover {
  border-color: var(--accent-line);
  transform: translateY(-1px);
}

.collection.on {
  border-color: var(--accent);
  background: var(--accent-soft);
}

.thumb {
  flex: none;
  width: 92px;
  height: 62px;
  overflow: hidden;
  background: var(--thumb);
  border: 1px solid var(--line-soft);
  border-radius: var(--radius-sm);
}

.thumb-count {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 2px;
  height: 100%;
  color: var(--accent-ink);
}

.thumb-count svg {
  width: 20px;
  height: 20px;
}

.thumb-count b {
  font-size: 15px;
}

.body {
  flex: 1;
  min-width: 0;
}

.ctitle {
  margin: 0;
  font-size: 14px;
  font-weight: 600;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.sub {
  margin: 4px 0 0;
  font-size: 11.5px;
  color: var(--faint);
}

.num {
  margin: 6px 0 0;
  font-size: 11.5px;
  color: var(--faint);
}

.pick {
  position: absolute;
  top: 10px;
  right: 10px;
  display: grid;
  place-items: center;
  width: 26px;
  height: 26px;
  color: var(--muted);
  background: var(--card);
  border: 1px solid var(--line);
  border-radius: 50%;
  cursor: pointer;
}

.pick svg {
  width: 15px;
  height: 15px;
}

.collection.on .pick {
  color: #fff;
  background: var(--accent);
  border-color: var(--accent);
}

.foot {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-top: 14px;
}

/* ── 登录后占满可用高度（与解析页「选择内容」视图同一套布局语言）：
   Tabs / 标题 / 搜索固定在顶部，卡片网格是唯一滚动区，
   底部统计栏恒在窗口下沿——不用滚到底才出现 ── */
.fill-height {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
}

.fill-height > .card {
  display: flex;
  flex: 1;
  flex-direction: column;
  min-height: 0;
  /* 圆角裁切 + 避免出现第二根页面滚动条 */
  overflow: hidden;
}

.fill-height .cards {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  /* 给滚动条与卡片悬停微倾留出边缘空间 */
  padding: 2px 6px 2px 2px;
  margin: 14px -6px 0 -2px;
}

/* 有列表时：底部统计升格为固定底栏（顶缘分割线，不随内容滚走） */
.fill-height .foot {
  flex: none;
  margin-top: 12px;
  padding-top: 12px;
  border-top: 1px solid var(--line-soft);
}

/* 空列表 / 读取提示：占满剩余区域，内容垂直居中 */
.fill-height .empty {
  flex: 1;
  min-height: 0;
  align-items: center;
}

.count {
  font-size: 12.5px;
  color: var(--muted);
}

.primary {
  padding: 8px 18px;
  font-weight: 600;
  color: #fff;
  background: var(--accent);
  border-radius: var(--radius-sm);
}

.primary:hover:not(:disabled) {
  background: var(--accent-dark);
}

.primary:disabled {
  opacity: 0.45;
  cursor: not-allowed;
}

.ghost {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 7px 12px;
  font-size: 12.5px;
  color: var(--text);
  background: var(--field);
  border: 1px solid var(--line);
  border-radius: var(--radius-sm);
}

.ghost:hover:not(:disabled) {
  border-color: var(--accent-line);
  background: var(--raised);
}

.empty {
  display: flex;
  align-items: center;
  gap: 18px;
  margin-top: 16px;
  padding: 30px 26px;
  background: var(--raised);
  border: 1px dashed var(--line);
  border-radius: var(--radius);
}

.empty.tall {
  justify-content: center;
  min-height: 320px;
}

.empty-icon {
  flex: none;
  display: grid;
  place-items: center;
  width: 44px;
  height: 44px;
  padding: 11px;
  color: var(--accent-ink);
  background: var(--accent-soft);
  border: 1px solid var(--accent-line);
  border-radius: 50%;
  animation: lib-float 3.2s ease-in-out infinite;
}

@keyframes lib-float {
  50% {
    transform: translateY(-4px);
  }
}

.empty-text {
  flex: 1;
  min-width: 0;
}

.empty-text .title {
  margin: 0;
  font-size: 14px;
  font-weight: 600;
}

.empty-text .hint {
  max-width: 62ch;
  margin: 6px 0 0;
  font-size: 12.5px;
  color: var(--muted);
  line-height: 1.7;
}
</style>

<style scoped>
/* 第六轮追加：勾选角标（+ → ✓）切换时轻弹 */
.collection.on .pick {
  animation: pick-pop 260ms var(--ease-out-expo);
}

@keyframes pick-pop {
  0% { transform: scale(0.8) rotate(-6deg); opacity: 0.4; }
  60% { transform: scale(1.08) rotate(2deg); opacity: 1; }
  100% { transform: none; opacity: 1; }
}
</style>
