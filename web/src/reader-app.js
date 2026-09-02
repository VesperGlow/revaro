import { assembleSegments } from './reader/segments.js'
import { findTocTarget, tocTargetPage } from './reader/navigation.js'
import { createProgressStore } from './reader/progress.js'
import { createReaderPreferences } from './reader/preferences.js'
import { fetchBookContent, fetchBookInfo, readerAPI } from './reader/api.js'

// ReaderApp: 自包含的阅读器模块（CSS 分栏分页版），原样移植自 VesperGlow/reader
// 的 reader.js，仅把 API 路径接到网盘（/api/files/:id/book/...）并去掉了
// 目录抽屉的历史栈操作（由网盘的弹窗历史接管返回键）。
// 渲染：把整本书的内容放进一个多栏容器（每栏=一屏），靠 translateX 切页，浏览器负责按行填满与重排。
// 同一会话内最多保留 2 本书的运行时缓存（keep-alive）。
export const ReaderApp = (function () {
  'use strict';

  const RUNTIME_LIMIT = 2;
  const MAX_COLUMN = 720; // 桌面宽屏时限制单栏文字宽度，保证可读性

  const runtime = new Map(); // bookId(string) -> state
  const order = [];          // LRU：末尾为最近使用

  let root = null;
  let els = {};
  let initialized = false; // 全局监听只绑一次
  let boundViewport = null; // 已绑定事件的骨架实例
  let active = null; // 当前展示的书 state
  let onExit = null;
  let resizeTimer = null;
  let suppressZoneClick = 0; // 滑动翻页后短暂吞掉热区补发的 click，避免一次滑动翻两页/误切工具栏
  const progressStore = createProgressStore(() => active, readerAPI);
  const { toggleTools, toggleTheme, setReaderFontSize, stepFontSize, loadPrefs } = createReaderPreferences(() => els, relayoutActive);

  // state: { bookId, info, kind, segments, pages, currentSeg, currentCol,
  //          currentX, currentTop, toc, tocEntries,
  //          currentPage, pageCount, pageWidth, pageHeight, restoreRatio }

  function init(rootElement) {
    // 每次挂载都重新抓取骨架元素：Vue 弹窗关闭时会销毁整个骨架，
    // 缓存里的 els 会指向已卸载的旧 DOM，必须重绑到新骨架。
    root = rootElement || document;
    const scope = root === document ? document : root;
    els = {
      view: scope.querySelector ? (scope.id === 'reader-view' ? scope : scope.querySelector('#reader-view')) : null,
      themeRoot: scope.querySelector && scope.querySelector('#reader-view') ? scope.querySelector('#reader-view') : (root === document ? document.body : root),
      title: scope.querySelector('#reader-title'),
      kind: scope.querySelector('#reader-kind'),
      back: scope.querySelector('#reader-back'),
      tocButton: scope.querySelector('#toc-button'),
      viewport: scope.querySelector('#viewport'),
      loading: scope.querySelector('#loading'),
      page: scope.querySelector('#reader-page'),
      prevZone: scope.querySelector('#prev-zone'),
      centerZone: scope.querySelector('#center-zone'),
      nextZone: scope.querySelector('#next-zone'),
      tocScrim: scope.querySelector('#toc-scrim'),
      tocDrawer: scope.querySelector('#toc-drawer'),
      tocClose: scope.querySelector('#toc-close'),
      tocList: scope.querySelector('#toc-list'),
      tocButton2: scope.querySelector('#toc-button-2'),
      themeButton: scope.querySelector('#theme-button'),
      pageLabel: scope.querySelector('#page-label'),
      slider: scope.querySelector('#page-slider'),
      fontButton: scope.querySelector('#font-button'),
      fontPopover: scope.querySelector('#font-popover'),
      fontSlider: scope.querySelector('#font-slider'),
      fontSmaller: scope.querySelector('#font-smaller'),
      fontLarger: scope.querySelector('#font-larger'),
    };
    if (els.viewport && els.viewport !== boundViewport) {
      bindElementEvents();
      boundViewport = els.viewport;
    }
    if (!initialized) {
      bindGlobalEvents();
      loadPrefs();
      initialized = true;
    }
  }

  function bindElementEvents() {
    const guardZone = handler => () => { if (Date.now() < suppressZoneClick) return; handler(); };
    els.prevZone && (els.prevZone.onclick = guardZone(previous));
    els.centerZone && (els.centerZone.onclick = guardZone(toggleTools));
    els.nextZone && (els.nextZone.onclick = guardZone(next));
    bindSwipe();
    els.tocButton && (els.tocButton.onclick = () => openToc());
    els.tocButton2 && (els.tocButton2.onclick = () => openToc());
    els.tocClose && (els.tocClose.onclick = () => closeToc());
    els.tocScrim && (els.tocScrim.onclick = () => closeToc());
    els.themeButton && (els.themeButton.onclick = toggleTheme);
    els.back && (els.back.onclick = event => { if (onExit) { event.preventDefault?.(); onExit(); } });
    els.slider && (els.slider.oninput = event => seekTo(Number(event.target.value), false));
    els.slider && (els.slider.onchange = () => active && queueProgressSave(active.currentPage));
    els.fontButton && (els.fontButton.onclick = () => els.fontPopover?.classList.toggle('hidden'));
    els.fontSlider && (els.fontSlider.oninput = event => setReaderFontSize(Number(event.target.value)));
    els.fontSmaller && (els.fontSmaller.onclick = () => stepFontSize(-1));
    els.fontLarger && (els.fontLarger.onclick = () => stepFontSize(1));
  }

  function bindGlobalEvents() {
    // 地址栏滑入/滑出（dvh 变化）有时只动 visualViewport 而不触发 window.resize，两个都听。
    // 仅在视口尺寸真正变化时才重排，避免 visualViewport 的杂音事件造成无谓重排；
    // 改字号引起的重排走 setReaderFontSize 直接调用，不经过这里。
    const scheduleRelayout = () => {
      if (active && els.viewport.clientWidth === active.pageWidth && els.viewport.clientHeight === active.pageHeight) return;
      clearTimeout(resizeTimer);
      resizeTimer = setTimeout(relayoutActive, 200);
    };
    window.addEventListener('resize', scheduleRelayout);
    window.visualViewport?.addEventListener('resize', scheduleRelayout);
    document.addEventListener('keydown', event => {
      if (!isVisible()) return;
      if (['ArrowLeft', 'PageUp'].includes(event.key)) previous();
      if (['ArrowRight', 'PageDown', ' '].includes(event.key)) next();
      if (event.key === 'Escape' && isTocOpen() && !window.Router) closeToc();
    });
    const flush = () => saveProgress();
    document.addEventListener('visibilitychange', () => { if (document.visibilityState === 'hidden') flush(); });
    window.addEventListener('pagehide', flush);
    window.addEventListener('beforeunload', flush);
    window.addEventListener('blur', flush);
    // 心跳保存：即使强刷/异常退出时 pagehide 的 keepalive 请求没送达，
    // 进度也最多落后 15 秒，不会整个丢失。
    setInterval(() => { if (active && isVisible()) saveProgress(); }, 15000);
  }

  // 左右滑动翻页（跟手拖动 + 松手吸附动画）。手指横向拖动时正文实时跟随，
  // 松手后按"快速轻扫或拖过 1/4 屏"判定翻页，否则弹回原页；边缘页拖动加阻尼。
  // 横向位移明显大于纵向才进入拖动，否则当作点击/纵向手势放行。
  function bindSwipe() {
    if (!els.viewport) return;
    let startX = 0, startY = 0, startTime = 0, tracking = false, dragging = false;

    const release = dx => {
      const flick = Date.now() - startTime < 300 && Math.abs(dx) > 30; // 快速轻扫：距离短也算翻页
      const shouldTurn = flick || Math.abs(dx) > active.pageStep * 0.25;
      const target = active.currentPage + (dx < 0 ? 1 : -1);
      if (shouldTurn && target >= 0 && target < active.pageCount) turnPage(dx < 0 ? 'next' : 'prev');
      else goToPage(active, active.currentPage, true); // 不够翻页：动画弹回
    };

    els.viewport.addEventListener('touchstart', event => {
      tracking = event.touches.length === 1 && !!active;
      dragging = false;
      if (tracking) {
        startX = event.touches[0].clientX;
        startY = event.touches[0].clientY;
        startTime = Date.now();
      }
    }, { passive: true });

    els.viewport.addEventListener('touchmove', event => {
      if (!tracking) return;
      if (event.touches.length !== 1) { tracking = false; if (dragging) goToPage(active, active.currentPage, true); return; }
      const dx = event.touches[0].clientX - startX;
      const dy = event.touches[0].clientY - startY;
      if (!dragging) {
        if (Math.abs(dx) < 8) return; // 起步死区：避免轻点抖动被当成拖动
        if (Math.abs(dx) < Math.abs(dy) * 1.2) { tracking = false; return; } // 偏纵向：放行
        dragging = true;
        suppressZoneClick = Date.now() + 600;
      }
      const atEdge = (active.currentPage === 0 && dx > 0) || (active.currentPage >= active.pageCount - 1 && dx < 0);
      setTransform(active, active.currentX + (atEdge ? dx / 3 : dx), false);
    }, { passive: true });

    els.viewport.addEventListener('touchend', event => {
      if (!tracking) return;
      tracking = false;
      const touch = event.changedTouches[0];
      const dx = touch.clientX - startX;
      const dy = touch.clientY - startY;
      if (!dragging) {
        // 没进入拖动就抬手（如被 iOS 系统手势打断）：沿用旧的一次性判定兜底
        if (Math.abs(dx) < 45 || Math.abs(dx) < Math.abs(dy) * 1.5) return;
        suppressZoneClick = Date.now() + 500;
      }
      release(dx);
    }, { passive: true });

    els.viewport.addEventListener('touchcancel', () => {
      if (!tracking) return;
      tracking = false;
      if (dragging) goToPage(active, active.currentPage, true);
    }, { passive: true });
  }

  function isVisible() {
    if (!active) return false;
    if (els.view) return !els.view.classList.contains('hidden');
    return true;
  }

  function show() {
    if (els.view) els.view.classList.remove('hidden');
    document.body.classList.add('reader-open');
  }

  function hide() {
    saveProgress();
    if (els.view) els.view.classList.add('hidden');
    document.body.classList.remove('reader-open');
  }

  // ---- 运行时缓存（keep-alive） ----
  function touchLRU(bookId) {
    const index = order.indexOf(bookId);
    if (index !== -1) order.splice(index, 1);
    order.push(bookId);
  }

  function evictLRU() {
    while (order.length > RUNTIME_LIMIT) {
      const oldest = order[0];
      if (oldest === active?.bookId) {
        order.push(order.shift());
        if (order[0] === active?.bookId) break;
        continue;
      }
      destroyBook(oldest);
    }
  }

  function destroyBook(bookId) {
    bookId = String(bookId);
    runtime.delete(bookId);
    const index = order.indexOf(bookId);
    if (index !== -1) order.splice(index, 1);
  }

  // ---- 打开书籍 ----
  async function openBook(bookId, options = {}) {
    init(options.root || document); // 每次都重抓骨架（弹窗重挂载后 DOM 是新实例）
    bookId = String(bookId);
    if (options.onExit) onExit = options.onExit;
    show();

    const reused = (active && active.bookId === bookId) ? active : runtime.get(bookId);
    if (reused) {
      console.info('[reader runtime] hit', { bookId });
      if (active && active !== reused) saveProgress();
      active = reused;
      touchLRU(bookId);
      applyActiveToDom();
      return;
    }
    if (active) saveProgress();

    console.info('[reader runtime] miss', { bookId });
    closeToc(true);
    setLoading('正在打开书页…');
    els.loading.classList.remove('hidden');
    try {
      const state = await loadBookState(bookId, options);
      runtime.set(bookId, state);
      touchLRU(bookId);
      active = state;
      evictLRU();
      await initializeSections(state);
      activate(state);
      renderToc();
      els.loading.classList.add('hidden');
      updatePageLabel();
      queueProgressSave(state.currentPage);
    } catch (error) {
      fail(error.message);
    }
  }

  function applyActiveToDom() {
    if (!active) return;
    setHeader(active.info);
    const mounted = els.page.firstElementChild === active.segments;
    if (!mounted || active.pageWidth !== els.viewport.clientWidth || active.pageHeight !== els.viewport.clientHeight) {
      activate(active);
    } else {
      goToPage(active, active.currentPage);
    }
    renderToc();
    els.loading.classList.add('hidden');
    updatePageLabel();
  }

  function setHeader(info) {
    els.title.textContent = info.title;
    els.kind.textContent = (info.kind || '').toUpperCase();
    document.title = `${info.title} · revaro`;
  }

  async function loadBookState(bookId, options) {
    const info = options.book || await fetchBookInfo(bookId);
    if (!info) throw new Error('书籍不存在');
    setHeader(info);
    const savedProgress = readerAPI(`/api/files/${bookId}/book/progress`)
      .then(response => response.json())
      .catch(() => ({ page: 0, total_pages: null }));

    setLoading('正在读取书籍…');
    // 解析已在服务端完成：EPUB 返回逐章清洗好的正文，TXT 返回原文 + 目录偏移。
    const model = await fetchBookContent(bookId);
    const segments = assembleSegments(model);
    const state = {
      bookId, info, kind: info.kind, segments,
      toc: model.toc || [], tocEntries: [],
      pages: [], currentSeg: null, currentCol: 0, currentX: 0, currentTop: 0,
      currentPage: 0, pageCount: 1, pageWidth: 0, pageHeight: 0, restoreRatio: 0,
    };
    const progress = await savedProgress;
    state.restoreRatio = progress && progress.total_pages > 1
      ? Math.min(1, Math.max(0, Number(progress.page) / (Number(progress.total_pages) - 1)))
      : 0;
    return state;
  }

  // ---- 分段分栏布局 / 翻页 ----
  // 每章一个分栏段：段们纵向堆叠在 wrapper 里，翻页只滑动当前段的小层
  // （章大小），跨章用 wrapper 的 top 定位瞬时切换；不再把整本书扛进
  // 一个数千页宽的巨型合成层——那是重型页面里翻页卡顿的根源。
  function activate(state) {
    if (els.page.firstElementChild !== state.segments) els.page.replaceChildren(state.segments);
    measure(state);
    if (state.restoreRatio != null) {
      state.currentPage = Math.round(state.restoreRatio * Math.max(0, state.pageCount - 1));
      state.restoreRatio = null;
    }
    goToPage(state, state.currentPage);
  }

  // EPUB 图片的固有尺寸必须在第一次分页前稳定。只在 section 初始化时
  // 等待网络 load/error；翻页路径不等待图片，也不再在阅读中重跑 measure()。
  async function initializeSections(state) {
    if (state.sectionsReady) return;
    if (els.page.firstElementChild !== state.segments) els.page.replaceChildren(state.segments);
    const pending = Array.from(state.segments.querySelectorAll('img')).filter(img => !img.complete);
    for (const img of pending) img.loading = 'eager';
    await Promise.all(pending.map(img => new Promise(resolve => {
      const done = () => resolve();
      img.addEventListener('load', done, { once: true });
      img.addEventListener('error', done, { once: true });
      if (img.complete) done();
    })));
    state.sectionsReady = true;
  }

  function measure(state) {
    cancelCurrentAnimation();
    const width = els.viewport.clientWidth;
    const height = els.viewport.clientHeight;
    let sidePad = Math.round(Math.min(Math.max(width * 0.055, 16), 44));
    if (width - 2 * sidePad > MAX_COLUMN) sidePad = Math.round((width - MAX_COLUMN) / 2);
    state.pageWidth = width;
    state.pageHeight = height;
    state.pageStep = Math.round(width);
    state.pages = [];
    for (const node of state.segments.children) {
      // 关键：把容器宽度钉成整数、box-sizing 内联强制，保证栏距严格 = 整数屏宽，
      // 否则浏览器用小数宽拉伸单栏，会让 translateX 逐页漂移、文字接不上。
      node.style.boxSizing = 'border-box';
      node.style.position = 'absolute';
      node.style.inset = '0 auto auto 0';
      node.style.width = `${width}px`;
      node.style.height = `${height}px`;
      // 手机阅读时按实际可视高度收紧页边距。工具栏是悬浮叠层，
      // 展开或收起都不改变正文尺寸与分页，避免阅读位置发生跳动。
      const mobile = width <= 850;
      const padTop = mobile
        ? Math.round(Math.min(28, Math.max(16, height * 0.025)))
        : 60;
      const padBottom = mobile
        ? Math.round(Math.min(22, Math.max(12, height * 0.018)))
        : 24;
      node.style.padding = `${padTop}px ${sidePad}px ${padBottom}px`;
      // 供 CSS 计算图片高度上限：栏高的精确像素值（图片常被包在 <p> 里，
      // 包含块高度为 auto，百分比 max-height 会被忽略，必须用像素值）
      node.style.setProperty('--reader-pad-y', `${padTop + padBottom}px`);
      node.style.setProperty('--reader-col-height', `${height - padTop - padBottom}px`);
      node.style.columnWidth = `${Math.max(1, width - 2 * sidePad)}px`;
      node.style.columnGap = `${2 * sidePad}px`;
      node.style.columnFill = 'auto';
      node.style.transition = 'none';
      node.style.transform = 'none';
      node.style.visibility = 'visible';
      node.style.background = 'var(--paper, #f7f3eb)';
      node._cols = Math.max(1, Math.round(node.scrollWidth / width));
      node._startPage = state.pages.length;
      for (let col = 0; col < node._cols; col++) state.pages.push({ node, col });
      node.style.visibility = 'hidden';
    }
    lastX = 0;
    state.currentSeg = null;
    state.pageCount = Math.max(1, state.pages.length);
    mapTocPages(state);
  }

  function goToPage(state, index, animate = false) {
    const page = Math.min(Math.max(0, index), Math.max(0, state.pageCount - 1));
    const previousPage = state.currentPage;
    state.currentPage = page;
    const slot = state.pages[page];
    const x = Math.round(-slot.col * state.pageStep);
    if (slot.node !== state.currentSeg) {
      // 跨 spine section 也保留翻页动效。图片页经常独占一个 section，
      // 旧的瞬时切换分支正是它看起来突然跳页的原因。
      const outgoingNode = state.currentSeg;
      const outgoingX = state.currentX;
      cancelCurrentAnimation();
      state.currentSeg = slot.node;
      state.currentCol = slot.col;
      state.currentX = x;
      slot.node.style.transition = 'none';
      slot.node.style.transform = `translateX(${x}px)`;
      slot.node.style.visibility = 'visible';
      slot.node.style.zIndex = '1';
      lastX = x;
      if (animate && page !== previousPage && outgoingNode) animateSectionTurn(state, outgoingNode, outgoingX, page > previousPage ? 1 : -1);
      else {
        releaseInactiveSections(state, outgoingNode);
      }
    } else {
      state.currentCol = slot.col;
      state.currentX = x;
      setTransform(state, x, animate);
    }
    updateTocActive();
  }

  function animateSectionTurn(state, outgoingNode, outgoingX, direction) {
    cancelCurrentAnimation();
    const incomingNode = state.currentSeg;
    const outgoingEnd = Math.round(outgoingX - direction * state.pageStep);
    const incomingStart = Math.round(state.currentX + direction * state.pageStep);
    outgoingNode.style.visibility = 'visible';
    outgoingNode.style.zIndex = '2';
    outgoingNode.style.willChange = 'transform';
    incomingNode.style.willChange = 'transform';
    const outgoingAnim = outgoingNode.animate(
      [{ transform: `translate3d(${Math.round(outgoingX)}px,0,0)` }, { transform: `translate3d(${outgoingEnd}px,0,0)` }],
      { duration: 260, easing: 'cubic-bezier(.22,.72,.26,1)' }
    );
    currentAnim = incomingNode.animate(
      [{ transform: `translate3d(${incomingStart}px,0,0)` }, { transform: `translate3d(${Math.round(state.currentX)}px,0,0)` }],
      { duration: 260, easing: 'cubic-bezier(.22,.72,.26,1)' }
    );
    currentAnimCleanup = () => {
      outgoingAnim.cancel();
      outgoingNode.style.visibility = 'hidden';
      outgoingNode.style.zIndex = '';
      outgoingNode.style.transform = 'none';
      outgoingNode.style.willChange = 'auto';
      incomingNode.style.zIndex = '1';
      incomingNode.style.willChange = 'auto';
    };
    currentAnim.onfinish = () => {
      currentAnim = null;
      currentAnimCleanup?.();
      currentAnimCleanup = null;
      releaseInactiveSections(state);
    };
  }

  function releaseInactiveSections(state, outgoingNode = null) {
    for (const node of state.segments.children) {
      if (node === state.currentSeg) continue;
      node.style.visibility = 'hidden';
      node.style.zIndex = '';
      node.style.willChange = 'auto';
      if (node === outgoingNode || node.style.transform !== 'none') node.style.transform = 'none';
    }
  }

  // 所有 transform 写入的唯一出口。动画用 WAAPI 从 JS 记录的当前位置直接
  // 插值：不读布局、不强制回流，全程合成器驱动——拖拽松手后的吸附翻页
  // 立即开始，不被排版阻塞。
  let lastX = 0;
  let currentAnim = null;
  let currentAnimCleanup = null;
  function cancelCurrentAnimation() {
    if (currentAnim) currentAnim.cancel();
    currentAnim = null;
    if (currentAnimCleanup) currentAnimCleanup();
    currentAnimCleanup = null;
  }
  function setTransform(state, x, animate) {
    const node = state.currentSeg;
    if (!node) return;
    cancelCurrentAnimation();
    node.style.transition = 'none';
    node.style.visibility = 'visible';
    node.style.transform = `translateX(${x}px)`;
    if (animate && x !== lastX) {
      currentAnim = node.animate(
        [{ transform: `translateX(${lastX}px)` }, { transform: `translateX(${x}px)` }],
        { duration: 260, easing: 'cubic-bezier(.22,.72,.26,1)' }
      );
      currentAnim.onfinish = () => { currentAnim = null; };
    }
    lastX = x;
  }

  function mapTocPages(state) {
    state.tocEntries = (state.toc || []).map((entry, index) => {
      const target = findTocTarget(state, entry, index);
      let page = 0;
      if (target) {
        const owner = target.closest('.book-content');
        if (owner && typeof owner._startPage === 'number') {
          page = owner._startPage + Math.max(0, Math.round((target.getBoundingClientRect().left - owner.getBoundingClientRect().left) / state.pageStep));
        }
      }
      return { ...entry, page: Math.min(page, state.pageCount - 1) };
    });
  }

  function turnPage(direction) {
    if (!active) return;
    const target = active.currentPage + (direction === 'next' ? 1 : -1);
    if (target < 0 || target >= active.pageCount) return;
    goToPage(active, target, true);
    updatePageLabel();
    queueProgressSave(active.currentPage);
  }

  function previous() { turnPage('prev'); }
  function next() { turnPage('next'); }

  function seekTo(page, save = true) {
    if (!active) return;
    goToPage(active, page);
    updatePageLabel();
    if (save) queueProgressSave(active.currentPage);
  }

  function relayoutActive() {
    if (!active || !isVisible()) return;
    // 重排按比例回到对应位置（分段模型下不做文字锚点对位）
    const fallbackRatio = active.pageCount > 1 ? active.currentPage / (active.pageCount - 1) : 0;
    measure(active);
    goToPage(active, Math.round(fallbackRatio * Math.max(0, active.pageCount - 1)));
    renderToc();
    updatePageLabel();
    queueProgressSave(active.currentPage);
  }

  // ---- 目录 ----
  let tocButtons = [];
  let lastTocActive = -1;
  function renderToc() {
    const list = els.tocList;
    const entries = active?.tocEntries || [];
    tocButtons = [];
    lastTocActive = -1;
    if (!entries.length) {
      list.innerHTML = `<p class="toc-empty">${active?.kind === 'txt' ? '没有识别到“第…章”格式的章节标题。' : '这本 EPUB 没有提供可用目录。'}</p>`;
      return;
    }
    list.innerHTML = entries.map((entry, index) => `<button class="toc-item" data-toc-index="${index}" style="--toc-indent:${Math.min(4, entry.depth || 0) * 16}px">${escapeHtml(entry.label)}</button>`).join('');
    tocButtons = [...list.querySelectorAll('[data-toc-index]')];
    tocButtons.forEach(button => button.onclick = () => {
      const index = Number(button.dataset.tocIndex);
      const entry = active.tocEntries[index];
      goToPage(active, tocTargetPage(active, entry, index));
      updatePageLabel();
      queueProgressSave(active.currentPage);
      closeToc();
    });
    updateTocActive();
  }

  function updateTocActive() {
    const entries = active?.tocEntries || [];
    if (!entries.length) return;
    let activeIndex = 0;
    for (let index = 0; index < entries.length; index++) if (entries[index].page <= active.currentPage) activeIndex = index;
    if (activeIndex === lastTocActive) return; // 翻页只在跨章时更新高亮，避免每次翻页重写整份目录
    lastTocActive = activeIndex;
    tocButtons.forEach((button, index) => button.classList.toggle('active', index === activeIndex));
  }

  function openToc() {
    if (isTocOpen()) return;
    els.tocDrawer.classList.add('open');
    els.tocDrawer.setAttribute('aria-hidden', 'false');
    els.tocButton.setAttribute('aria-expanded', 'true');
    els.tocScrim.classList.remove('hidden');
  }

  function closeToc() {
    if (!isTocOpen()) return;
    els.tocDrawer.classList.remove('open');
    els.tocDrawer.setAttribute('aria-hidden', 'true');
    els.tocButton.setAttribute('aria-expanded', 'false');
    els.tocScrim.classList.add('hidden');
  }

  function isTocOpen() { return els.tocDrawer?.classList.contains('open'); }

  function updatePageLabel() {
    if (!active) return;
    const percentage = active.pageCount <= 1 ? 100 : Math.round((active.currentPage / (active.pageCount - 1)) * 100);
    els.pageLabel.textContent = `${active.currentPage + 1} / ${active.pageCount} · ${percentage}%`;
    if (els.slider) {
      els.slider.max = Math.max(0, active.pageCount - 1);
      els.slider.value = active.currentPage;
    }
  }

  // 翻页时只防抖排期，不立即写；停下 PROGRESS_SAVE_DELAY 后落一次，把连续翻页合并成一次写入。
  function queueProgressSave(page) {
    progressStore.queue(page);
  }

  // 离开页面/切书等场景立即落盘（绕过防抖），确保不丢进度。
  function saveProgress() {
    progressStore.save();
  }

  function setLoading(message) { els.loading.textContent = message; }
  function fail(message) { els.loading.classList.remove('hidden'); setLoading(message); }
  function escapeHtml(value) { const div = document.createElement('div'); div.textContent = value; return div.innerHTML; }

  window.setReaderFontSize = setReaderFontSize;

  function getState() {
    return active ? { bookId: active.bookId, page: active.currentPage } : null;
  }

  async function restoreState(state) {
    if (!state || !state.bookId) return;
    await openBook(state.bookId, {});
    if (active && Number.isFinite(state.page)) {
      goToPage(active, state.page);
      updatePageLabel();
    }
  }

  // 兼容入口：单独打开 reader.html?id=1（网盘内不启用）
  if (document.body.classList.contains('reading-body')) {
    const id = new URLSearchParams(location.search).get('id');
    init(document.body);
    if (id) openBook(id, { standalone: true });
    else fail('缺少书籍编号');
  }

  return { init, openBook, hide, show, saveProgress, getState, restoreState, destroyBook, isTocOpen, closeToc };
})();
