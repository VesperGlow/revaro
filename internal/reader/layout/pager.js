// Revaro 服务端分页脚本（在 headless Chromium 内运行）。
// 输入：整本清洗后的 spine 已由 Go 装入 #revaro-book，每章一个
// .spine > .revaro-content；共享样式表与锁定参数由 Go 内联注入。
// 输出：window.__revaroPaginate() → Promise<result>，result 由
// chromedp Evaluate 序列化回 Go。
//
// 算法（与旧客户端同源的多栏排版）：
//   1. 对每章内容根建立「步进索引」：文本节点每个字符偏移一个 caret，
//      元素首尾各一个边界 caret，按文档序排列，连续重复去重；
//   2. 每栏一个页：栏 c 的结束边界 = 第一个使 [栏首, s) 内容溢出的
//      caret（Range.getBoundingClientRect().right 越界判定，二分查找）；
//   3. 页切片 = Range.cloneContents()，天然闭合、跨页元素完整；
//   4. 每页包装成锁定 viewport/字体/行距/边距的 .revaro-page，
//      序列化为固定 Page HTML。
//
// 锚点（readingAnchor）：caret 的 (spine, path, offset) 三元组；元素边界
// caret 的 offset 记为 -1。同一 DOM 上确定性生成，任何 profile 都一致。

(() => {
  'use strict';

  const MAX_PAGES = 20000;
  const EPS = 0.5; // 亚像素容差
  const UNBREAKABLE = new Set(['H1', 'H2', 'H3', 'H4', 'FIGURE', 'TABLE', 'PRE', 'IMG', 'SVG', 'VIDEO']);

  function indexOf(parent, node) {
    const kids = parent.childNodes;
    for (let i = 0; i < kids.length; i++) if (kids[i] === node) return i;
    return -1;
  }

  // buildSteps 建立章内容根（.revaro-content）的步进索引。
  // step: { node, offset, boundary, path }，path 为相对内容根的节点路径。
  // 同时记录不可断元素（break-inside:avoid）在步进索引里的 [enter, exit]
  // 区间，供边界回退使用：空壳元素（如被推到下一栏的标题）其盒不计入
  // Range 矩形，二分会落在元素文本内部，必须回退到元素之前才不撕开它。
  function buildSteps(root) {
    const steps = [];
    const unbreakables = [];
    function walk(node, path) {
      if (node.nodeType === Node.TEXT_NODE) {
        for (let o = 0; o <= node.data.length; o++) {
          steps.push({ node, offset: o, boundary: false, path });
        }
      } else if (node.nodeType === Node.ELEMENT_NODE) {
        const idx = indexOf(node.parentNode, node);
        const parentPath = path.slice(0, -1);
        const enter = steps.length;
        steps.push({ node: node.parentNode, offset: idx, boundary: true, path });
        const kids = node.childNodes;
        for (let i = 0; i < kids.length; i++) {
          walk(kids[i], path.concat([indexOf(node, kids[i])]));
        }
        const exit = steps.length;
        steps.push({ node: node.parentNode, offset: idx + 1, boundary: true, path: parentPath.concat([idx + 1]) });
        if (UNBREAKABLE.has(node.tagName)) unbreakables.push({ start: enter, end: exit });
      }
    }
    const kids = root.childNodes;
    for (let i = 0; i < kids.length; i++) walk(kids[i], [indexOf(root, kids[i])]);
    // 连续重复的 (node, offset) caret 只保留一个；区间索引随之修正。
    // 被去重掉的 enter 步与其保留的孪生步（前一兄弟的 exit）位置等价。
    const out = [];
    const shift = new Array(steps.length).fill(0);
    const droppedAt = new Array(steps.length).fill(false);
    let dropped = 0;
    for (let i = 0; i < steps.length; i++) {
      const s = steps[i];
      shift[i] = dropped;
      const prev = out[out.length - 1];
      if (prev && prev.node === s.node && prev.offset === s.offset) { dropped++; droppedAt[i] = true; continue; }
      out.push(s);
    }
    const intervals = [];
    for (const iv of unbreakables) {
      intervals.push({
        start: droppedAt[iv.start] ? (iv.start - 1) - shift[iv.start - 1] : iv.start - shift[iv.start],
        end: droppedAt[iv.end] ? (iv.end - 1) - shift[iv.end - 1] : iv.end - shift[iv.end],
      });
    }
    return { steps: out, unbreakables: intervals };
  }

  // retreat 把边界回退到最内层「严格包含」它的不可断元素之前。
  function retreat(boundary, unbreakables) {
    let s = boundary;
    let changed = true;
    while (changed) {
      changed = false;
      for (const iv of unbreakables) {
        if (s > iv.start && s < iv.end) { s = iv.start; changed = true; }
      }
    }
    return s;
  }

  // fits：范围 [steps[lo], steps[s]) 的内容是否不超出 limitRight（栏右缘）。
  function fits(steps, lo, s, limitRight) {
    const range = document.createRange();
    range.setStart(steps[lo].node, steps[lo].offset);
    range.setEnd(steps[s].node, steps[s].offset);
    const rect = range.getBoundingClientRect();
    if (rect.width === 0) return true; // 空范围（caret 自身）不占空间
    return rect.right <= limitRight + EPS;
  }

  // findBoundary：本栏的结束 caret = 最后一个使 [lo, s) 内容不溢出栏的 s。
  // 必须取「最后一个 fits」（而非第一个溢出）：不可断元素（table/figure/img
  // 等 break-inside:avoid）被推到下一栏时，其内部所有 caret 都已溢出，
  // 第一个溢出的 caret 会落在元素内部；最后一个 fits 的 caret 恰好是
  // 元素之前的位置，切片才不会把不可断元素撕开。
  function findBoundary(steps, lo, limitRight) {
    const hi = steps.length - 1;
    if (fits(steps, lo, hi, limitRight)) return hi; // 余下内容都在本栏
    let low = lo, high = hi;
    while (low < high) {
      const mid = Math.ceil((low + high) / 2);
      if (fits(steps, lo, mid, limitRight)) low = mid;
      else high = mid - 1;
    }
    return low;
  }

  function anchorOf(spine, step) {
    return { spine, path: step.path, offset: step.boundary ? -1 : step.offset };
  }

  // slicePage 把 [steps[lo], steps[hi]) 切成一个锁定参数的固定页。
  function slicePage(profile, spine, index, lo, hi, steps) {
    const range = document.createRange();
    range.setStart(steps[lo].node, steps[lo].offset);
    range.setEnd(steps[hi].node, steps[hi].offset);
    const frag = range.cloneContents();

    const wrap = document.createElement('div');
    wrap.className = 'revaro-page';
    wrap.setAttribute('data-spine', String(spine));
    wrap.setAttribute('data-index', String(index));
    const innerH = profile.viewportH - profile.marginTop - profile.marginBottom;
    wrap.style.width = profile.viewportW + 'px';
    wrap.style.height = profile.viewportH + 'px';
    wrap.style.padding = profile.marginTop + 'px ' + profile.marginSide + 'px ' + profile.marginBottom + 'px';
    wrap.style.setProperty('--revaro-font-family', profile.fontFamily);
    wrap.style.setProperty('--revaro-font-size', profile.fontSize + 'px');
    wrap.style.setProperty('--revaro-line-height', String(profile.lineHeight));
    wrap.style.setProperty('--revaro-col-height', innerH + 'px');

    const content = document.createElement('div');
    content.className = 'revaro-content' + (profile.txt ? ' txt' : '');
    content.setAttribute('data-spine', String(spine));
    content.appendChild(frag);
    wrap.appendChild(content);
    return wrap.outerHTML;
  }

  async function paginate(profile) {
    const result = { ok: false, error: '', pages: [], sections: [], toc: [], diagnostics: [] };
    try {
      await document.fonts.ready;
      await Promise.all(Array.from(document.images).map(img => img.decode().catch(() => {})));
      void document.body.offsetWidth; // 强制一次布局，确保多栏几何就绪

      const book = document.getElementById('revaro-book');
      const sections = Array.from(book.querySelectorAll('.spine'));
      const stepsBySpine = {};
      const rangesBySpine = {}; // spine -> [{index, start, end}]（页的 step 区间）
      let pageIndex = 0;
      for (const section of sections) {
        const spine = Number(section.dataset.spine);
        const root = section.querySelector('.revaro-content');
        if (!root) { result.diagnostics.push({ type: 'missing-root', spine }); continue; }
        const built = buildSteps(root);
        const steps = built.steps;
        const unbreakables = built.unbreakables;
        stepsBySpine[spine] = steps;
        if (steps.length === 0) { result.diagnostics.push({ type: 'empty-section', spine }); continue; }

        const rect = section.getBoundingClientRect();
        if (rect.width <= 0) { result.diagnostics.push({ type: 'zero-size', spine }); continue; }
        const cols = Math.max(1, Math.round(section.scrollWidth / rect.width));
        const cs = getComputedStyle(section);
        const colW = parseFloat(cs.columnWidth) || 1;
        const gap = parseFloat(cs.columnGap) || 0;
        const padLeft = parseFloat(cs.paddingLeft) || 0;

        // bounds[c] = 栏 c 的起始 caret；bounds[cols] = 章末 caret。
        const bounds = [0];
        for (let c = 1; c < cols; c++) {
          const limitRight = rect.left + padLeft + c * colW + (c - 1) * gap;
          bounds.push(retreat(findBoundary(steps, bounds[c - 1], limitRight), unbreakables));
        }
        bounds.push(steps.length - 1);

        const ranges = [];
        let prev = 0;
        let colPages = 0;
        for (let c = 0; c < cols; c++) {
          const b = Math.max(bounds[c], prev);
          const e = Math.max(bounds[c + 1], b);
          if (e <= b) { result.diagnostics.push({ type: 'empty-column', spine, col: c }); continue; }
          prev = e;
          if (pageIndex >= MAX_PAGES) {
            result.error = `页数超过 ${MAX_PAGES} 上限`;
            return result;
          }
          const html = slicePage(profile, spine, pageIndex, b, e, steps);
          result.pages.push({
            spine,
            index: pageIndex,
            start: anchorOf(spine, steps[b]),
            end: anchorOf(spine, steps[e]),
            html,
          });
          ranges.push({ index: pageIndex, start: b, end: e });
          pageIndex++;
          colPages++;
        }
        rangesBySpine[spine] = ranges;
        result.sections.push({ spine, cols, pages: colPages });
      }
      result.toc = resolveTOC(profile, sections, stepsBySpine, rangesBySpine);
      result.ok = true;
    } catch (err) {
      result.error = String((err && err.stack) || err);
    }
    return result;
  }

  // resolveTOC 把目录目标（元素）映射到页码：元素的前置 caret 落在哪一页的
  // step 区间内，就属于哪一页。找不到元素时回退到目标章的第一页。
  function resolveTOC(profile, sections, stepsBySpine, rangesBySpine) {
    const out = [];
    for (const target of (profile.toc || [])) {
      let page = 0;
      const ranges = rangesBySpine[target.spine] || [];
      if (ranges.length) page = ranges[0].index; // 默认：目标章第一页
      const section = sections.find(s => Number(s.dataset.spine) === target.spine);
      const steps = stepsBySpine[target.spine];
      if (section && steps && ranges.length) {
        let el = null;
        if (target.kind === 'toc-anchor') {
          el = section.querySelector('[data-toc="' + target.index + '"]');
        } else if (target.kind === 'fragment' && target.fragment) {
          el = Array.from(section.querySelectorAll('[id], [data-frag-ids]')).find(e =>
            e.id === target.fragment || (e.dataset.fragIds || '').split(' ').includes(target.fragment)
          ) || null;
        }
        if (el) {
          const parent = el.parentNode;
          const off = indexOf(parent, el);
          let s = -1;
          for (let k = 0; k < steps.length; k++) {
            if (steps[k].node === parent && steps[k].offset === off && steps[k].boundary) { s = k; break; }
          }
          if (s >= 0) {
            for (const r of ranges) {
              if (s >= r.start && s < r.end) { page = r.index; break; }
            }
          }
        }
      }
      out.push({ index: target.index, page });
    }
    return out;
  }

  window.__revaroPaginate = () => paginate(window.__revaroProfile || {});
})();
