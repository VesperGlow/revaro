// Adapted from Kirikiroid2 Web 2028cbcc; see SOURCE.md and ENGINE-LICENSE.txt.
var statusElement = document.getElementById('status');
        var progressFill = document.getElementById('progress-fill');
        var loadingOverlay = document.getElementById('loading');
        var filePicker = document.getElementById('file-picker');
        var pickerStatus = document.getElementById('picker-status');
        var pickerProgress = document.getElementById('picker-progress');
        var pickerProgressFill = document.getElementById('picker-progress-fill');
        var engineStatusElement = document.getElementById('engine-status');
        function setEngineStatus(text) {
            engineStatusElement.textContent = text;
        }
        var fsReadyResolve;
var fsReady = new Promise(function(resolve) { fsReadyResolve = resolve; });
var gameStartPending = false;
var gameFileReady = false;
        var userFileDepAdded = false;
        var hasFileSystemAPI = typeof window.showDirectoryPicker === 'function';

        // --- Web 平台边界：同一 origin 只允许一个活跃引擎实例 ---
        // VLFS 的 OPFS 临时缓存按页面会话隔离，但两个引擎实例仍会争用全局
        // 运行资源和持久化存档空间。Web Locks 的持有期与 Document 生命周期
        // 绑定，标签页关闭/崩溃后浏览器会自动释放，不需要心跳或超时猜测。
        var resolveEngineSingleton;
        var engineSingletonReady = new Promise(function(resolve) {
            resolveEngineSingleton = resolve;
        });

        function showEngineSingletonConflict() {
            document.querySelector('#error-box h2').textContent =
                'Engine Already Running';
            document.getElementById('error-message').textContent =
                '同一站点已有另一个 KrKr2 引擎窗口正在运行。请关闭另一个窗口后刷新本页。\n' +
                'Another KrKr2 engine tab is already running for this site. Close it, then reload this page.';
            document.getElementById('error-update-btn').style.display = 'none';
            document.getElementById('error-overlay').classList.add('visible');
            setEngineStatus('Another engine tab is already running');
        }

        if (navigator.locks && typeof navigator.locks.request === 'function') {
            navigator.locks.request(
                'krkr2-web-engine-instance',
                { mode: 'exclusive', ifAvailable: true },
                function(lock) {
                    if (!lock) {
                        showEngineSingletonConflict();
                        resolveEngineSingleton(false);
                        return;
                    }
                    resolveEngineSingleton(true);
                    // 保持 callback pending 即持有锁；Document 销毁时自动释放。
                    return new Promise(function() {});
                }
            ).catch(function(error) {
                console.error('[singleton] Web Lock request failed:', error);
                showEngineSingletonConflict();
                resolveEngineSingleton(false);
            });
        } else {
            // 目标 Chromium 137+ 均支持 Web Locks；仅在其他实现缺少该 API 时
            // 保持原有单窗口行为，避免把平台能力缺失误报成已有实例。
            console.warn('[singleton] Web Locks unavailable; cross-tab guard disabled');
            resolveEngineSingleton(true);
        }

        // Revaro only reloads this player; application caches and workers are retained.
        var BUILD_VERSION = 'revaro-2028cbcc-v4-text';
        if (!BUILD_VERSION || BUILD_VERSION.charAt(0) === '@') BUILD_VERSION = 'dev';
        document.getElementById('build-version').textContent = 'Build ' + BUILD_VERSION;
        function forceUpdate() { window.location.reload(); }
        document.getElementById('force-update-btn').addEventListener('click', function (e) {
            e.preventDefault();
            if (confirm('重新加载播放器？浏览器存档将保留。')) forceUpdate();
        });
        // --- File System Access API state ---
        var hostDirHandle = null;
        var webdavWriteChain = Promise.resolve();
        let webdavContext = null;

        function isSaveStoragePath(path) {
            return path.startsWith('/savedata/') || path.startsWith('/save/') ||
                path.indexOf('/savedata/') >= 0 || path.indexOf('/save/') >= 0;
        }

        function queueWebDavSaveWrite(path, data) {
            var context = webdavContext;
            if (!context || !context.writeEnabled) return;
            var copy = data.slice();
            webdavWriteChain = webdavWriteChain.then(function() {
                return VLFS.writeWebDavFile(
                    context.rootUrl, context.requestHeaders, path, copy,
                    context.knownDirs);
            }).then(function() {
                console.log('[webdav] save uploaded: ' + path);
            }).catch(function(err) {
                // 吞掉本次错误以保持队列可继续处理后续存档写入；本地 overlay
                // 与可选 IDB 副本仍然保留。
                console.warn('[webdav] save upload FAILED: ' + path +
                             ' - ' + err.message);
            });
        }

        function queueWebDavSaveDelete(path) {
            var context = webdavContext;
            if (!context || !context.writeEnabled) return;
            webdavWriteChain = webdavWriteChain.then(function() {
                return VLFS.deleteWebDavFile(
                    context.rootUrl, context.requestHeaders, path);
            }).then(function() {
                console.log('[webdav] save deleted: ' + path);
            }).catch(function(err) {
                console.warn('[webdav] save delete FAILED: ' + path +
                             ' - ' + err.message);
            });
        }

        // --- VirtualLazyFS 初始化（创建独立 OPFS 会话、保留完整 ZIP 缓存）---
        // 必须在取得跨标签页单例锁后才接触 OPFS。冲突页让此 Promise 保持
        // pending，因此不会继续创建 OPFS 会话、注册 ZIP 或启动 wasm main。
        var vlfsReady = engineSingletonReady.then(function(acquired) {
            if (!acquired) return new Promise(function() {});
            return VLFS.init();
        }).catch(function(e) {
            console.error('[vlfs] init failed:', e);
        });

        // VLFS 写关闭钩子：统一持久化链路（取代旧 fsafs_flush_file）。
        // 1) MEMFS 小文件镜像 —— 兼容遗留 fopen 直读（GlobalConfigManager 等）；
        // 2) IndexedDB write-through —— 存档空间持久化；
        // 3) 可选 WebDAV 存档回写；4) FSA 主机目录回写。
        VLFS.onWriteClose = function(path, data) {
            if (data.length <= 8 * 1024 * 1024 && typeof FS !== 'undefined') {
                try { writeFileToFS(path, data); } catch (e) {}
            }
            // 游戏可能把存档写在游戏目录内的 savedata/ 下（如千恋万花
            // mainwindow.tjs checkSave），一并纳入 IDB 持久化
            var isSavePath = isSaveStoragePath(path);
            if (isSavePath && webdavContext && webdavContext.writeEnabled) {
                queueWebDavSaveWrite(path, data);
            }
            if (!Module._hostDirHandle) {
                if (Module._saveSpaceId && isSavePath) idbSaveFile(path, data);
                return;
            }
            var dirHandle = Module._hostDirHandle;
            var prefix = Module._hostDirPrefix;
            var copy = data.slice();
            setTimeout(async function() {
                try {
                    var relPath = path;
                    if (prefix && relPath.startsWith(prefix + '/')) {
                        relPath = relPath.substring(prefix.length);
                    }
                    var parts = relPath.split('/').filter(Boolean);
                    var fileName = parts.pop();
                    var cur = dirHandle;
                    for (var i = 0; i < parts.length; i++) {
                        cur = await cur.getDirectoryHandle(parts[i], { create: true });
                    }
                    var fh = await cur.getFileHandle(fileName, { create: true });
                    var writable = await fh.createWritable();
                    await writable.write(copy);
                    await writable.close();
                } catch (err) {
                    console.warn('[vlfs] FSA write-back FAILED: ' + path + ' - ' + err.message);
                }
            }, 0);
        };

        VLFS.onUnlink = function(path) {
            if (isSaveStoragePath(path) && webdavContext &&
                webdavContext.writeEnabled) {
                queueWebDavSaveDelete(path);
            }
        };

        // Keep native dialog behavior, but mirror alert/confirm content
        // to the console for easier debugging.
        (function() {
            var origAlert = window.alert;
            var origConfirm = window.confirm;
            window.alert = function(msg) {
                console.warn('[alert]', msg);
                return origAlert.call(window, msg);
            };
            window.confirm = function(msg) {
                console.warn('[confirm]', msg);
                var result = origConfirm.call(window, msg);
                console.warn('[confirm result]', result);
                return result;
            };
        })();

        // --- UI 资源/字体（assets.zip, store 模式打包）注册进 VLFS ---
        // 取代 --preload-file index.data：资源以 Blob 切片按需读取
        // （cocos CCFileUtils 经 krkr2_vlfs_read_all 桥访问），不驻留内存。
        var assetsLoaded = (async function() {
            await vlfsReady;
            var resp = await fetch('assets.zip?v=revaro-2028cbcc-v4-text');
            if (!resp.ok) throw new Error('assets.zip HTTP ' + resp.status);
            var blob = await resp.blob();
            var r = await VLFS.registerZipBlob(blob, { stripPrefix: '' });
            console.log('[vlfs] assets.zip registered: ' + r.paths.length + ' entries');
        })().catch(function(e) {
            console.error('[vlfs] assets.zip load failed:', e);
        });

        // --- 共享 wasm 内存预创建（iOS Safari 兼容）---
        // glue 的 initMemory 默认 new WebAssembly.Memory({maximum: 2GB, shared:true})。
        // V8 对 maximum 只做惰性虚拟地址预留，而 JSC（尤其 iOS）按 maximum 实预留，
        // iPhone 的 WebContent 进程拿不到 2GB，直接 RangeError: Out of memory，
        // 引擎死在启动前。glue 优先采用 Module.wasmMemory（initMemory 第一分支），
        // 这里逐级降档预创建：桌面第一档 2048MB 即成功（行为与原来完全一致），
        // iOS 自动落到能分配的档位。提供的 maximum ≤ 模块声明的 2GB 可通过导入校验。
        var preallocWasmMemory = null;
        (function () {
            // 由 CMake 经 configure_file @ONLY 注入，与链接参数 -s INITIAL_MEMORY
            // 同源（KRKR2_WEB_INITIAL_MEMORY）。ASan 构建需 1GB（影子内存把 wasm
            // 声明的 min 顶高），非 ASan 64MB。预分配的 initial 必须 >= wasm 声明
            // 的 min，否则 glue 采用本 Memory 时导入校验失败 → LinkError。
            var INITIAL_PAGES = 67108864 / 65536;
            var ladder = [2048, 1536, 1024, 768];  // MB
            for (var i = 0; i < ladder.length; i++) {
                try {
                    preallocWasmMemory = new WebAssembly.Memory({
                        initial: INITIAL_PAGES,
                        maximum: ladder[i] * 16,   // 1MB = 16 个 64KB 页
                        shared: true
                    });
                    if (i > 0) console.warn('[mem] shared memory maximum 降档至 ' + ladder[i] + 'MB（首选 2048MB 分配失败）');
                    break;
                } catch (e) { /* 尝试下一档 */ }
            }
            if (!preallocWasmMemory) {
                document.querySelector('#error-box h2').textContent = 'Out of Memory';
                document.getElementById('error-message').textContent =
                    '无法分配引擎所需的共享内存，请关闭其他标签页/应用后刷新重试。\n' +
                    'Failed to allocate shared WebAssembly memory. Close other tabs/apps and reload.';
                document.getElementById('error-overlay').classList.add('visible');
            }
        })();

        var Module = {
            wasmMemory: preallocWasmMemory || undefined,
            locateFile: function(path, prefix) {
                if (path === 'index.wasm')
                    return prefix + 'index.wasm?v=revaro-2028cbcc-v4-text';
                if (path === 'index.worker.js')
                    return prefix + 'index.worker.js?v=revaro-2028cbcc-v4-text';
                return prefix + path;
            },
            preRun: [function() {
                try { FS.mkdir('/savedata'); } catch (e) { if (!FS.analyzePath('/savedata').exists) throw e; }
                applyRendererPreferenceFromUrl();
                Module.addRunDependency('engine-singleton');
                engineSingletonReady.then(function(acquired) {
                    if (acquired) Module.removeRunDependency('engine-singleton');
                });
                Module.addRunDependency('user-file');
                userFileDepAdded = true;

                fsReadyResolve();
                // UI 资源注册完成前引擎不得启动（cocos init 需要字体/UI）
                Module.addRunDependency('vlfs-assets');
                assetsLoaded.then(function() {
                    Module.removeRunDependency('vlfs-assets');
                });
            }],
            postRun: [function() {
                loadingOverlay.classList.add('hidden');
            }],
            print: function(text) { console.log(text); },
            printErr: function(text) { console.error(text); },
            canvas: (function() {
                var canvas = document.getElementById('canvas');
                canvas.addEventListener('webglcontextlost', function(e) {
                    alert('WebGL context lost. Please reload the page.');
                    e.preventDefault();
                }, false);
                return canvas;
            })(),
            setStatus: function(text) {
                if (!text) {
                    loadingOverlay.classList.add('hidden');
                    if (!gameFileReady && !xp3Url && !gameZipUrl && !webdavUrl) {
                        showFilePicker();
                    }
                    return;
                }
                var m = text.match(/([^(]+)\((\d+(\.\d+)?)\/(\d+)\)/);
                if (m) {
                    var pct = (parseInt(m[2]) / parseInt(m[4])) * 100;
                    progressFill.style.width = pct + '%';
                    statusElement.textContent = m[1].trim();
                    if (!gameFileReady) setEngineStatus(m[1].trim() + ' ' + Math.round(pct) + '%');
                } else {
                    statusElement.textContent = text;
                    if (!gameFileReady) setEngineStatus(text);
                }
            },
            onAbort: function(what) {
                document.getElementById('error-message').textContent =
                    'The application has aborted. ' + what;
                document.getElementById('error-overlay').classList.add('visible');
            },
            monitorRunDependencies: function(left) {
                if (left === 0) {
                    Module.setStatus('');
                } else if (!gameFileReady) {
                    if (userFileDepAdded && left === 1) {
                        // Only the 'user-file' dependency remains: the engine itself
                        // is ready and waiting for the user to pick a game.
                        setEngineStatus('Engine ready');
                    } else {
                        Module.setStatus('Preparing engine... (' + left + ' remaining)');
                    }
                } else {
                    Module.setStatus('Starting... (' + left + ' remaining)');
                }
            }
        };

        Module.setStatus('Downloading engine...');

        window.onerror = function(event) {
            Module.setStatus('Error: see console');
        };

        (function() {
            var canvas = document.getElementById('canvas');
            var container = document.getElementById('container');
            function updateCanvasSize() {
                var dpr = window.devicePixelRatio || 1;
                var cssW = Math.round(container.clientWidth) || 1280;
                var cssH = Math.round(container.clientHeight) || 720;
                var w = Math.round(cssW * dpr);
                var h = Math.round(cssH * dpr);
                if (canvas.width !== w || canvas.height !== h) {
                    canvas.width = w;
                    canvas.height = h;
                }
            }
            updateCanvasSize();
            new ResizeObserver(updateCanvasSize).observe(container);
            window.addEventListener('resize', updateCanvasSize);
            if (window.matchMedia) {
                var mqDpr = window.matchMedia('(resolution: ' + window.devicePixelRatio + 'dppx)');
                if (mqDpr.addEventListener) {
                    mqDpr.addEventListener('change', updateCanvasSize);
                }
            }
        })();

        var searchUrlParams = new URLSearchParams(window.location.search);
        var urlParams = searchUrlParams;
        var xp3Url = null;
        var gameZipUrl = window.REVARO_GAME.archive;
        var webdavUrl = null;
        var entryXp3 = window.REVARO_GAME.entry;
        var loadMode = 'lazy';
        var webdavWrite = normalizeBooleanUrlParam(
            urlParams.get('webdavWrite'), false, 'webdavWrite');
        let webdavCredentials = window.__krkr2WebDavCredentials;
        const webdavRequestHeaders = {};
        if (webdavCredentials) {
            webdavRequestHeaders.Authorization = makeBasicAuthHeader(
                webdavCredentials.username, webdavCredentials.password);
            webdavCredentials.username = '';
            webdavCredentials.password = '';
            webdavCredentials = null;
            window.__krkr2WebDavCredentials = null;
        }
        var rendererUrlParam = normalizeRendererUrlParam(urlParams.get('renderer'));
        var localZipPickerEnabled = false || urlParams.has('pickZip');

        function normalizeLoadModeUrlParam(value) {
            if (value === null || value === '') return 'eager';
            value = String(value).trim().toLowerCase();
            if (value === 'eager' || value === 'lazy') return value;
            console.warn('[loader] Ignoring unsupported URL loadMode:', value);
            return 'eager';
        }

        function normalizeBooleanUrlParam(value, fallback, name) {
            if (value === null || value === '') return fallback;
            value = String(value).trim().toLowerCase();
            if (value === '1' || value === 'true' || value === 'yes' || value === 'on')
                return true;
            if (value === '0' || value === 'false' || value === 'no' || value === 'off')
                return false;
            console.warn('[loader] Ignoring unsupported URL ' + name + ':', value);
            return fallback;
        }

        function makeBasicAuthHeader(username, password) {
            var bytes = new TextEncoder().encode(
                String(username || '') + ':' + String(password || ''));
            var binary = '';
            for (var i = 0; i < bytes.length; i++)
                binary += String.fromCharCode(bytes[i]);
            return 'Basic ' + btoa(binary);
        }

        function normalizeRendererUrlParam(value) {
            if (value === null) return '';
            value = String(value).trim().toLowerCase();
            if (value === 'software' || value === 'opengl') return value;
            console.warn('[renderer] Ignoring unsupported URL renderer:', value);
            return '';
        }

        function rendererPreferenceXml(existingXml, renderer) {
            var doc = null;
            if (existingXml) {
                try {
                    doc = new DOMParser().parseFromString(existingXml, 'application/xml');
                    if (doc.getElementsByTagName('parsererror').length ||
                        !doc.documentElement ||
                        doc.documentElement.tagName !== 'GlobalPreference') {
                        doc = null;
                    }
                } catch (e) {
                    doc = null;
                }
            }
            if (!doc) {
                doc = document.implementation.createDocument('', 'GlobalPreference', null);
            }

            var root = doc.documentElement;
            var items = root.getElementsByTagName('Item');
            var rendererItem = null;
            for (var i = 0; i < items.length; i++) {
                if (items[i].getAttribute('key') === 'renderer') {
                    rendererItem = items[i];
                    break;
                }
            }
            if (!rendererItem) {
                rendererItem = doc.createElement('Item');
                rendererItem.setAttribute('key', 'renderer');
                root.appendChild(rendererItem);
            }
            rendererItem.setAttribute('value', renderer);
            return new XMLSerializer().serializeToString(doc);
        }

        function applyRendererPreferenceFromUrl() {
            if (!rendererUrlParam || typeof FS === 'undefined') return;
            try {
                ensureDir('/save');
                var path = '/save/GlobalPreference.xml';
                var existing = '';
                try {
                    existing = FS.readFile(path, { encoding: 'utf8' });
                } catch (e) {}
                var xml = rendererPreferenceXml(existing, rendererUrlParam);
                FS.writeFile(path, xml, { encoding: 'utf8' });
                // VLFS overlay 同步（引擎 TVP_stat/读流以 VLFS 优先）
                VLFS.registerOverlayFile(path, new TextEncoder().encode(xml));
                console.log('[renderer] URL override applied:', rendererUrlParam);
            } catch (e) {
                console.warn('[renderer] Failed to apply URL renderer override:', e);
            }
        }

        if (localZipPickerEnabled) {
            document.getElementById('pick-zip-btn').style.display = '';
        }

        // Show File System API button when supported
        if (hasFileSystemAPI) {
            document.getElementById('pick-dir-api-btn').style.display = '';
            document.getElementById('picker-divider').style.display = '';
        }

        // --- IndexedDB save space persistence ---
        var IDB_PREFIX = 'krkr2-space-';
        var currentIdb = null;

        function idbOpen(spaceId) {
            return new Promise(function(resolve, reject) {
                var req = indexedDB.open(IDB_PREFIX + spaceId, 1);
                req.onupgradeneeded = function(e) { e.target.result.createObjectStore('files'); };
                req.onsuccess = function(e) { currentIdb = e.target.result; resolve(currentIdb); };
                req.onerror = function() { reject(req.error); };
            });
        }

        function idbSaveFile(path, data) {
            if (!currentIdb) return;
            try {
                var tx = currentIdb.transaction('files', 'readwrite');
                tx.objectStore('files').put(new Uint8Array(data), path);
            } catch (e) { console.warn('[IDB] save failed:', path, e); }
        }

        function idbLoadAll() {
            if (!currentIdb) return Promise.resolve([]);
            return new Promise(function(resolve) {
                var tx = currentIdb.transaction('files', 'readonly');
                var store = tx.objectStore('files');
                var results = [];
                var cursorReq = store.openCursor();
                cursorReq.onsuccess = function(e) {
                    var cursor = e.target.result;
                    if (cursor) {
                        results.push({ path: cursor.key, data: cursor.value });
                        cursor.continue();
                    } else { resolve(results); }
                };
                cursorReq.onerror = function() { resolve(results); };
            });
        }

        function idbListSpaces() {
            if (indexedDB.databases) {
                return indexedDB.databases().then(function(dbs) {
                    return dbs
                        .filter(function(d) { return d.name && d.name.startsWith(IDB_PREFIX); })
                        .map(function(d) { return d.name.substring(IDB_PREFIX.length); });
                });
            }
            var saved = localStorage.getItem('krkr2-spaces');
            return Promise.resolve(saved ? JSON.parse(saved) : []);
        }

        function idbRegisterSpace(name) {
            try {
                var saved = JSON.parse(localStorage.getItem('krkr2-spaces') || '[]');
                if (saved.indexOf(name) < 0) { saved.push(name); localStorage.setItem('krkr2-spaces', JSON.stringify(saved)); }
            } catch (e) {}
        }

        function idbUnregisterSpace(name) {
            try {
                var saved = JSON.parse(localStorage.getItem('krkr2-spaces') || '[]');
                saved = saved.filter(function(s) { return s !== name; });
                localStorage.setItem('krkr2-spaces', JSON.stringify(saved));
            } catch (e) {}
        }

        function idbDeleteSpace(spaceId) {
            return new Promise(function(resolve) {
                var req = indexedDB.deleteDatabase(IDB_PREFIX + spaceId);
                req.onsuccess = function() { idbUnregisterSpace(spaceId); resolve(); };
                req.onerror = function() { resolve(); };
            });
        }

        function idbGetSpaceInfo(spaceId) {
            return new Promise(function(resolve) {
                var req = indexedDB.open(IDB_PREFIX + spaceId, 1);
                req.onupgradeneeded = function(e) { e.target.result.createObjectStore('files'); };
                req.onsuccess = function(e) {
                    var db = e.target.result;
                    try {
                        var tx = db.transaction('files', 'readonly');
                        var store = tx.objectStore('files');
                        var count = 0, totalSize = 0;
                        var cur = store.openCursor();
                        cur.onsuccess = function(ev) {
                            var c = ev.target.result;
                            if (c) { count++; totalSize += c.value.byteLength || 0; c.continue(); }
                            else { db.close(); resolve({ count: count, size: totalSize }); }
                        };
                        cur.onerror = function() { db.close(); resolve({ count: 0, size: 0 }); };
                    } catch (err) { db.close(); resolve({ count: 0, size: 0 }); }
                };
                req.onerror = function() { resolve({ count: 0, size: 0 }); };
            });
        }

        async function idbRestoreSaves() {
            var files = await idbLoadAll();
            if (files.length === 0) return;
            await waitForFS();
            for (var i = 0; i < files.length; i++) {
                // MEMFS 镜像（遗留 fopen 读）+ VLFS overlay（引擎读流优先走 VLFS）
                writeFileToFS(files[i].path, files[i].data);
                VLFS.registerOverlayFile(files[i].path, new Uint8Array(files[i].data));
            }
            console.log('[IDB] Restored ' + files.length + ' save file(s)');
        }

        async function idbExportZip(spaceId) {
            var db = await new Promise(function(resolve, reject) {
                var req = indexedDB.open(IDB_PREFIX + spaceId, 1);
                req.onupgradeneeded = function(e) { e.target.result.createObjectStore('files'); };
                req.onsuccess = function(e) { resolve(e.target.result); };
                req.onerror = function() { reject(req.error); };
            });
            var files = await new Promise(function(resolve) {
                var tx = db.transaction('files', 'readonly');
                var results = [];
                var cur = tx.objectStore('files').openCursor();
                cur.onsuccess = function(e) {
                    var c = e.target.result;
                    if (c) { results.push({ path: c.key, data: c.value }); c.continue(); }
                    else { resolve(results); }
                };
                cur.onerror = function() { resolve(results); };
            });
            db.close();
            var zip = new JSZip();
            for (var i = 0; i < files.length; i++) {
                zip.file(files[i].path.replace(/^\//, ''), files[i].data);
            }
            var blob = await zip.generateAsync({ type: 'blob' });
            var a = document.createElement('a');
            a.href = URL.createObjectURL(blob);
            a.download = spaceId + '-saves.zip';
            a.click();
            URL.revokeObjectURL(a.href);
        }

        async function idbImportZip(file) {
            var zip = await JSZip.loadAsync(file);
            var spaceName = file.name.replace(/\.zip$/i, '').replace(/-saves$/, '');
            await idbOpen(spaceName);
            idbRegisterSpace(spaceName);
            var entries = [];
            zip.forEach(function(p, e) { if (!e.dir) entries.push({ path: p, entry: e }); });
            for (var i = 0; i < entries.length; i++) {
                var data = await entries[i].entry.async('uint8array');
                var path = '/' + entries[i].path;
                idbSaveFile(path, data);
            }
            if (currentIdb) { currentIdb.close(); currentIdb = null; }
            return spaceName;
        }

        // --- Save space picker UI ---
        var saveSpacePicker = document.getElementById('save-space-picker');
        var spaceStatus = document.getElementById('space-status');

        function formatSize(bytes) {
            if (bytes < 1024) return bytes + ' B';
            if (bytes < 1048576) return (bytes / 1024).toFixed(1) + ' KB';
            return (bytes / 1048576).toFixed(1) + ' MB';
        }

        async function refreshSpaceList() {
            var listEl = document.getElementById('space-list');
            listEl.innerHTML = '';
            spaceStatus.textContent = 'Loading...';
            var spaces = await idbListSpaces();
            spaceStatus.textContent = '';
            if (spaces.length === 0) {
                listEl.innerHTML = '<div style="color:#666;font-size:13px;padding:12px;">No save spaces yet. Create one below.</div>';
                return;
            }
            for (var i = 0; i < spaces.length; i++) {
                (function(name) {
                    var item = document.createElement('div');
                    item.className = 'space-item';
                    var info = document.createElement('div');
                    info.className = 'space-info';
                    info.innerHTML = '<div class="space-name">' + name + '</div><div class="space-meta">Loading...</div>';
                    info.addEventListener('click', function() { selectSpace(name); });
                    var exportBtn = document.createElement('button');
                    exportBtn.textContent = 'Export';
                    exportBtn.addEventListener('click', function() {
                        spaceStatus.textContent = 'Exporting...';
                        idbExportZip(name).then(function() { spaceStatus.textContent = 'Exported!'; });
                    });
                    var deleteBtn = document.createElement('button');
                    deleteBtn.className = 'delete-btn';
                    deleteBtn.textContent = 'Delete';
                    deleteBtn.addEventListener('click', function() {
                        if (confirm('Delete save space "' + name + '"? This cannot be undone.')) {
                            idbDeleteSpace(name).then(refreshSpaceList);
                        }
                    });
                    item.appendChild(info);
                    item.appendChild(exportBtn);
                    item.appendChild(deleteBtn);
                    listEl.appendChild(item);
                    idbGetSpaceInfo(name).then(function(si) {
                        info.querySelector('.space-meta').textContent = si.count + ' file(s), ' + formatSize(si.size);
                    });
                })(spaces[i]);
            }
        }

            var spaceChosen = false;
        var pendingNonFsaAction = null;

        async function selectSpace(name) {
            Module._saveSpaceId = name;
            localStorage.setItem('krkr2-last-space', name);
            await idbOpen(name);
            idbRegisterSpace(name);
            spaceChosen = true;
            saveSpacePicker.classList.remove('visible');
            filePicker.classList.add('visible');
            if (pendingNonFsaAction) {
                var action = pendingNonFsaAction;
                pendingNonFsaAction = null;
                action();
            }
        }

        function showFilePicker() {
            // Auto-load failed or finished without a game: re-enable the
            // statically rendered landing page hidden by the early head script.
            document.documentElement.classList.remove('krkr2-autoload');
            filePicker.classList.add('visible');
        }

        function showSpacePicker() {
            refreshSpaceList();
            saveSpacePicker.classList.add('visible');
        }

        function ensureSpaceThen(action) {
            if (spaceChosen) { action(); return; }
            pendingNonFsaAction = action;
            filePicker.classList.remove('visible');
            showSpacePicker();
        }

        document.getElementById('space-create-btn').addEventListener('click', function() {
            var input = document.getElementById('space-name-input');
            var name = input.value.trim();
            if (!name) return;
            input.value = '';
            selectSpace(name);
        });

        document.getElementById('space-name-input').addEventListener('keydown', function(e) {
            if (e.key === 'Enter') document.getElementById('space-create-btn').click();
        });

        document.getElementById('space-skip-btn').addEventListener('click', function() {
            Module._saveSpaceId = null;
            spaceChosen = true;
            saveSpacePicker.classList.remove('visible');
            filePicker.classList.add('visible');
            if (pendingNonFsaAction) {
                var action = pendingNonFsaAction;
                pendingNonFsaAction = null;
                action();
            }
        });

        document.getElementById('space-import-btn').addEventListener('click', function() {
            document.getElementById('space-import-input').click();
        });

        document.getElementById('space-import-input').addEventListener('change', function(e) {
            var file = e.target.files[0];
            if (!file) return;
            e.target.value = '';
            spaceStatus.textContent = 'Importing...';
            idbImportZip(file).then(function(name) {
                spaceStatus.textContent = 'Imported "' + name + '"';
                refreshSpaceList();
            }).catch(function(err) {
                spaceStatus.textContent = 'Import failed: ' + err.message;
            });
        });

        // --- Common helpers ---
        function waitForFS() {
            return new Promise(function(resolve) {
                if (typeof FS !== 'undefined') { resolve(); return; }
                var check = setInterval(function() {
                    if (typeof FS !== 'undefined') { clearInterval(check); resolve(); }
                }, 50);
            });
        }

        function ensureDir(path) {
            var parts = path.split('/').filter(Boolean);
            var cur = '';
            for (var i = 0; i < parts.length; i++) {
                cur += '/' + parts[i];
                try { FS.mkdir(cur); } catch(e) {}
            }
        }

        function writeFileToFS(fullPath, data) {
            var dir = fullPath.substring(0, fullPath.lastIndexOf('/'));
            if (dir) ensureDir(dir);
            FS.writeFile(fullPath, data);
        }

        function setPickerProgress(pct, text) {
            pickerProgress.classList.add('visible');
            pickerProgressFill.style.width = pct + '%';
            if (text) pickerStatus.textContent = text;
        }

        function showXp3Selector(xp3Paths, dirPrefix, callback) {
            var selectorEl = document.getElementById('xp3-selector');
            var listEl = document.getElementById('xp3-list');
            listEl.innerHTML = '';

            xp3Paths.sort(function(a, b) {
                return a.toLowerCase().localeCompare(b.toLowerCase());
            });

            for (var i = 0; i < xp3Paths.length; i++) {
                (function(path) {
                    var name = path.substring(path.lastIndexOf('/') + 1);
                    var dir = path.substring(0, path.lastIndexOf('/'));
                    var relDir = dirPrefix ? dir.substring(dirPrefix.length) : dir;

                    var btn = document.createElement('button');
                    btn.className = 'xp3-item';
                    var nameEl = document.createElement('div');
                    nameEl.className = 'xp3-name';
                    nameEl.textContent = name;
                    btn.appendChild(nameEl);
                    if (relDir) {
                        var dirEl = document.createElement('div');
                        dirEl.className = 'xp3-dir';
                        dirEl.textContent = relDir;
                        btn.appendChild(dirEl);
                    }
                    btn.addEventListener('click', function() {
                        selectorEl.classList.remove('visible');
                        callback(path);
                    });
                    listEl.appendChild(btn);
                })(xp3Paths[i]);
            }

            filePicker.classList.remove('visible');
            pickerProgress.classList.remove('visible');
            selectorEl.classList.add('visible');
        }

        function checkAndStartGame(xp3Paths, dirPrefix) {
            if (xp3Paths.length > 1) {
                showXp3Selector(xp3Paths, dirPrefix, function(path) {
                    Module._startupXp3Path = path;
                    startGame();
                });
            } else {
                startGame();
            }
        }

        async function startGame() {
            if (gameStartPending) return;
            gameStartPending = true;
            filePicker.classList.remove('visible');
            statusElement.textContent = 'Restoring saves...';
            try {
                await fsReady;
                await idbRestoreSaves();
                applyRendererPreferenceFromUrl();
                gameFileReady = true;
                if (userFileDepAdded) {
                    userFileDepAdded = false;
                    Module.removeRunDependency('user-file');
                }
            } catch (error) { failGame(error); }
        }

        function failGame(error) {
            loadingOverlay.classList.add('hidden');
            document.getElementById('error-message').textContent = String(error.message || error);
            document.getElementById('error-overlay').classList.add('visible');
        }

        // --- VLFS 加载器：注册元数据即可玩，无全量下载/解压/复制 ---

        // 流式下载为 Blob，带进度且 JS 堆峰值有界：每 64MB 块段合并为
        // 段 Blob（Blob 拼接引用而非复制），下载中堆驻留 ≤64MB。
        async function responseBlobWithProgress(resp, onProgress) {
            var total = parseInt(resp.headers.get('Content-Length') || '0', 10);
            if (!resp.body) return await resp.blob();
            var reader = resp.body.getReader();
            var segments = [], chunks = [], chunkBytes = 0, loaded = 0;
            var SEGMENT = 64 * 1024 * 1024;
            while (true) {
                var r = await reader.read();
                if (r.done) break;
                chunks.push(r.value);
                chunkBytes += r.value.length;
                loaded += r.value.length;
                if (onProgress) onProgress(loaded, total);
                if (chunkBytes >= SEGMENT) {
                    segments.push(new Blob(chunks));
                    chunks = [];
                    chunkBytes = 0;
                }
            }
            if (chunks.length) segments.push(new Blob(chunks));
            return new Blob(segments);
        }

        async function fetchBlobWithProgress(url, onProgress, requestHeaders) {
            var resp = await fetch(url, { headers: requestHeaders || {} });
            if (!resp.ok) throw new Error('HTTP ' + resp.status + ' ' + resp.statusText);
            return await responseBlobWithProgress(resp, onProgress);
        }

        function strongArchiveETag(rawETag) {
            var etag = String(rawETag || '').trim();
            if (!etag || /^W\//i.test(etag) || !/^"[^"]*"$/.test(etag))
                return '';
            return etag;
        }

        function archiveResourceKey(url) {
            var resource = new URL(url, document.baseURI);
            resource.hash = '';
            return resource.toString();
        }

        // 游戏 ZIP 不经 Service Worker/HTTP cache：完整文件的唯一持久副本
        // 在 OPFS。条件 GET 的 304 由本层与该 OPFS 副本配对。
        async function fetchArchiveWithProgress(url, onProgress, requestHeaders) {
            var resp = await fetch(url, {
                headers: requestHeaders || {}, cache: 'no-store'
            });
            var etag = strongArchiveETag(resp.headers.get('ETag'));
            if (resp.status === 304)
                return { status: 304, etag: etag, blob: null };
            if (!resp.ok)
                throw new Error('HTTP ' + resp.status + ' ' + resp.statusText);
            return {
                status: resp.status,
                etag: etag,
                blob: await responseBlobWithProgress(resp, onProgress)
            };
        }

        // ?xp3=：优先 HTTP Range 懒加载；服务器不支持时整包 Blob（仍 off-heap）
        async function fetchXp3FromUrl(url) {
            loadingOverlay.classList.remove('hidden');
            statusElement.textContent = 'Probing game data...';
            progressFill.style.width = '0%';
            try {
                await vlfsReady;
                var ranges = false, size = -1;
                try {
                    var head = await fetch(url, { method: 'HEAD' });
                    if (head.ok) {
                        ranges = (head.headers.get('Accept-Ranges') || '').toLowerCase() === 'bytes';
                        size = parseInt(head.headers.get('Content-Length') || '-1', 10);
                    }
                } catch (e) {}

                if (ranges && size > 0) {
                    VLFS.registerRemote('/data.xp3', url, size, true);
                    console.log('[vlfs] remote xp3 (Range): ' + url + ', ' + size + ' bytes');
                } else {
                    statusElement.textContent = 'Downloading game data...';
                    var blob = await fetchBlobWithProgress(url, function(loaded, total) {
                        if (total > 0) {
                            var pct = Math.round(loaded / total * 100);
                            progressFill.style.width = pct + '%';
                            statusElement.textContent = 'Downloading... ' + pct + '%';
                        } else {
                            statusElement.textContent = 'Downloading... ' + (loaded / 1048576).toFixed(1) + ' MB';
                        }
                    });
                    VLFS.registerBlobFile('/data.xp3', blob);
                    console.log('[vlfs] remote xp3 (whole blob): ' + blob.size + ' bytes');
                }
                startGame();
            } catch (err) {
                statusElement.textContent = 'Download failed: ' + err.message;
                progressFill.style.width = '0%';
                console.error('fetchXp3FromUrl error:', err);
                failGame(err);
            }
        }

        // ?webdavUrl=：将远程 collection 映射到 VLFS 根目录。loadMode=eager
        // 预下载全部文件；lazy 只注册 URL/尺寸，首次读取时再请求文件内容。
        async function fetchWebDavGame(rootUrl, entryFile) {
            loadingOverlay.classList.remove('hidden');
            statusElement.textContent = 'Scanning WebDAV directory...';
            progressFill.style.width = '0%';
            try {
                await vlfsReady;
                var eagerFiles = loadMode === 'eager';
                var reg = await VLFS.registerWebDav(
                    rootUrl, webdavRequestHeaders, {
                        eagerFiles: eagerFiles,
                        onProgress: function(phase, done, total, path) {
                            if (phase === 'scan') {
                                progressFill.style.width =
                                    Math.min(25, 5 + done) + '%';
                                statusElement.textContent =
                                    'Scanning WebDAV (' + done + ' directories) ' +
                                    (path || '/');
                            } else if (phase === 'download') {
                                var pct = total ? Math.round(done / total * 75) : 0;
                                progressFill.style.width = (25 + pct) + '%';
                                statusElement.textContent =
                                    'Downloading WebDAV files (' + done + '/' +
                                    total + ') ' + String(path || '').substring(1);
                            }
                        }
                    });

                webdavContext = {
                    rootUrl: reg.rootUrl,
                    requestHeaders: reg.requestHeaders,
                    knownDirs: reg.knownDirs,
                    writeEnabled: webdavWrite
                };
                console.log('[webdav] registered: ' + reg.paths.length +
                            ' files, ' + reg.xp3Paths.length + ' xp3, loadMode=' +
                            loadMode + ', write=' + webdavWrite);
                progressFill.style.width = '100%';
                statusElement.textContent = 'Starting game...';

                if (entryFile) {
                    var entryLower = entryFile.toLowerCase();
                    var matched = reg.xp3Paths.find(function(path) {
                        return path.substring(path.lastIndexOf('/') + 1)
                            .toLowerCase() === entryLower;
                    });
                    if (!matched) throw new Error('入口 XP3 不存在：' + entryFile);
                    Module._startupXp3Path = matched;
                    startGame();
                } else {
                    checkAndStartGame(reg.xp3Paths, '');
                }
            } catch (err) {
                statusElement.textContent = 'WebDAV failed: ' + err.message;
                progressFill.style.width = '0%';
                console.error('fetchWebDavGame error:', err);
                failGame(err);
            }
        }

        // ?game=：eager 用条件 GET 校验完整 OPFS 副本；304 直接恢复，
        // 200 则把该 GET 自身的强 ETag 与实际 Blob 绑定，再将
        // stored/deflate 全部写入 OPFS。lazy 仍保持 Range 按需读取。
        async function fetchAndExtractZip(url, entryFile) {
            loadingOverlay.classList.remove('hidden');
            statusElement.textContent = loadMode === 'lazy' ?
                'Probing game archive...' : 'Downloading game archive...';
            progressFill.style.width = '0%';
            try {
                await vlfsReady;
                var eagerDeflate = loadMode === 'eager';
                var resourceKey = archiveResourceKey(url);
                var reg;
                if (eagerDeflate) {
                    var cachedETag = await VLFS.getZipCacheValidator(resourceKey);
                    var requestHeaders = cachedETag ?
                        { 'If-None-Match': cachedETag } : {};
                    var downloaded;
                    try {
                        downloaded = await fetchArchiveWithProgress(
                            url, function(loaded, total) {
                                if (total > 0) {
                                    var pct = Math.round(loaded / total * 100);
                                    progressFill.style.width = (pct * 0.6) + '%';
                                    statusElement.textContent = 'Downloading... ' +
                                        pct + '% (' +
                                        (loaded / 1048576).toFixed(1) + ' MB)';
                                } else {
                                    statusElement.textContent = 'Downloading... ' +
                                        (loaded / 1048576).toFixed(1) + ' MB';
                                }
                            }, requestHeaders);
                    } catch (conditionalError) {
                        if (!cachedETag) throw conditionalError;
                        // 跨域服务器可能不允许 If-None-Match 预检；
                        // 无条件 GET 仍可用其实际响应 ETag 安全重建。
                        console.warn('[vlfs] conditional ZIP request failed; ' +
                            'retrying unconditionally:', conditionalError);
                        downloaded = await fetchArchiveWithProgress(url, null, {});
                    }

                    if (downloaded.status === 304) {
                        var validatedETag = downloaded.etag || cachedETag;
                        if (validatedETag === cachedETag) {
                            statusElement.textContent = 'Restoring cached archive...';
                            progressFill.style.width = '50%';
                            reg = await VLFS.restoreZipCache(
                                resourceKey, cachedETag);
                        }
                        if (!reg) {
                            console.warn('[vlfs] validated ZIP cache is unavailable; ' +
                                'downloading a fresh archive');
                            downloaded = await fetchArchiveWithProgress(url, null, {});
                        }
                    }

                    if (!reg) throw new Error('服务器未提供可用的 HTTP Range，已停止加载以避免整包下载');
                    if (!reg) {
                        statusElement.textContent = 'Caching archive...';
                        progressFill.style.width = '60%';
                        reg = await VLFS.registerZipBlob(downloaded.blob, {
                            eagerDeflate: true,
                            persistentCache: {
                                resourceKey: resourceKey,
                                etag: downloaded.etag
                            },
                            onProgress: function(done, total, path) {
                                progressFill.style.width =
                                    (60 + Math.round(done / total * 40)) + '%';
                                statusElement.textContent =
                                    'Caching (' + done + '/' + total + ') ' +
                                    path.substring(1);
                            }
                        });
                        console.log('[vlfs] remote ZIP cached from GET: ' +
                            downloaded.blob.size + ' bytes, etag=' +
                            (downloaded.etag || '(none)'));
                    }
                } else {
                    var ranges = false, size = -1, archiveETag = '';
                    try {
                        var head = await fetch(url, {
                            method: 'HEAD', cache: 'no-store'
                        });
                        if (head.ok) {
                            ranges = (head.headers.get('Accept-Ranges') || '')
                                .toLowerCase() === 'bytes';
                            size = parseInt(
                                head.headers.get('Content-Length') || '-1', 10);
                            archiveETag = strongArchiveETag(
                                head.headers.get('ETag'));
                        }
                    } catch (e) {}

                    if (ranges && size > 0) {
                        statusElement.textContent = 'Reading archive index...';
                        progressFill.style.width = '5%';
                        try {
                            reg = await VLFS.registerZipRemote(url, size, {
                                eagerDeflate: false,
                                etag: archiveETag || undefined
                            });
                            console.log('[vlfs] remote zip index mounted: ' + url +
                                        ', ' + size + ' bytes, loadMode=lazy');
                        } catch (rangeError) {
                            throw rangeError;
                        }
                    }
                    if (!reg) throw new Error('服务器未提供可用的 HTTP Range，已停止加载以避免整包下载');
                    if (!reg) {
                        statusElement.textContent = 'Downloading game archive...';
                        var fallback = await fetchArchiveWithProgress(
                            url, function(loaded, total) {
                                if (total > 0) {
                                    var pct = Math.round(loaded / total * 100);
                                    progressFill.style.width = pct * 0.95 + '%';
                                    statusElement.textContent = 'Downloading... ' +
                                        pct + '% (' +
                                        (loaded / 1048576).toFixed(1) + ' MB)';
                                }
                            }, {});
                        statusElement.textContent = 'Indexing archive...';
                        progressFill.style.width = '95%';
                        reg = await VLFS.registerZipBlob(fallback.blob, {
                            eagerDeflate: false
                        });
                        console.log('[vlfs] remote zip blob: ' +
                            fallback.blob.size +
                            ' bytes, loadMode=lazy (Range unavailable)');
                    }
                }
                console.log('[vlfs] zip registered: ' + reg.paths.length + ' entries, ' +
                            reg.xp3Paths.length + ' xp3');

                progressFill.style.width = '100%';
                statusElement.textContent = 'Starting game...';

                if (entryFile) {
                    var entryLower = entryFile.toLowerCase();
                    var matched = reg.xp3Paths.find(function(p) {
                        return p.replace(/^\//, '').toLowerCase() === entryLower;
                    });
                    if (!matched) throw new Error('入口 XP3 不存在：' + entryFile);
                    Module._startupXp3Path = matched;
                    startGame();
                } else if (reg.xp3Paths.length > 1) {
                    checkAndStartGame(reg.xp3Paths, '');
                } else {
                    startGame();
                }
            } catch (err) {
                statusElement.textContent = 'Failed: ' + err.message;
                progressFill.style.width = '0%';
                console.error('fetchAndExtractZip error:', err);
                failGame(err);
            }
        }

        async function autoSelectSpace() {
            Module._saveSpaceId = window.REVARO_GAME.save_space;
            await idbOpen(Module._saveSpaceId);
            if (navigator.storage && navigator.storage.persist) {
                navigator.storage.persist().catch(function() {});
            }
        }

        if (webdavUrl) {
            autoSelectSpace().then(function() {
                fetchWebDavGame(webdavUrl, entryXp3);
            });
        } else if (gameZipUrl) {
            autoSelectSpace().then(function() { fetchAndExtractZip(gameZipUrl, entryXp3); }).catch(failGame);
        } else if (xp3Url) {
            autoSelectSpace().then(function() { fetchXp3FromUrl(xp3Url); });
        }

        // --- File System Access API: directory picker + save sync ---
        async function enumerateDir(dirHandle, prefix) {
            var files = [];
            for await (var entry of dirHandle.values()) {
                var path = prefix + '/' + entry.name;
                if (entry.kind === 'directory') {
                    files = files.concat(await enumerateDir(entry, path));
                } else {
                    files.push({ path: path, handle: entry });
                }
            }
            return files;
        }

        async function handleDirectoryAPI() {
            try {
                hostDirHandle = await window.showDirectoryPicker({ mode: 'readwrite' });
            } catch (err) {
                if (err.name !== 'AbortError')
                    pickerStatus.textContent = 'Error: ' + err.message;
                return;
            }

            try {
                setPickerProgress(0, 'Scanning directory...');
                await vlfsReady;
                var dirPrefix = '/' + hostDirHandle.name;
                var entries = await enumerateDir(hostDirHandle, dirPrefix);
                var total = entries.length;
                if (total === 0) {
                    pickerStatus.textContent = 'Directory is empty.';
                    pickerProgress.classList.remove('visible');
                    hostDirHandle = null;
                    return;
                }

                Module._hostDirHandle = hostDirHandle;
                Module._hostDirPrefix = dirPrefix;

                var xp3Paths = [];
                for (var i = 0; i < total; i++) {
                    var entry = entries[i];
                    var innerPath = entry.path.substring(dirPrefix.length);
                    var lowerName = innerPath.substring(innerPath.lastIndexOf('/') + 1).toLowerCase();

                    setPickerProgress(
                        Math.round(i / total * 100),
                        'Indexing (' + (i + 1) + '/' + total + ') ' + innerPath.substring(1)
                    );

                    // 仅注册句柄+尺寸元数据，内容按需经 FSA 切片读取
                    await VLFS.registerFSAFileEager(innerPath, entry.handle);
                    if (lowerName.endsWith('.xp3')) xp3Paths.push(innerPath);
                }

                setPickerProgress(100, 'Starting...');
                checkAndStartGame(xp3Paths, '');
            } catch (err) {
                pickerStatus.textContent = 'Error: ' + err.message;
                pickerProgress.classList.remove('visible');
                hostDirHandle = null;
            }
        }

        // --- Fallback: .xp3 file picker ---
        async function handleXp3File(file) {
            try {
                setPickerProgress(50, 'Indexing ' + file.name + '...');
                await vlfsReady;
                // File 即 Blob：零拷贝注册，按需切片读取
                VLFS.registerBlobFile('/data.xp3', file);
                setPickerProgress(100, 'Starting...');
                startGame();
            } catch (err) {
                pickerStatus.textContent = 'Error: ' + err.message;
                pickerProgress.classList.remove('visible');
            }
        }

        async function handleZipFile(file) {
            try {
                setPickerProgress(10, 'Indexing ' + file.name + '...');
                await vlfsReady;
                var eagerDeflate = loadMode === 'eager';
                var reg = await VLFS.registerZipBlob(file, {
                    eagerDeflate: eagerDeflate,
                    onProgress: eagerDeflate ? function(done, total, path) {
                        setPickerProgress(10 + Math.round(done / total * 90),
                            'Extracting (' + done + '/' + total + ') ' + path.substring(1));
                    } : null
                });
                console.log('[vlfs] local zip registered: ' + reg.paths.length +
                            ' entries, loadMode=' + loadMode);
                setPickerProgress(100, 'Starting...');
                checkAndStartGame(reg.xp3Paths, '');
            } catch (err) {
                pickerStatus.textContent = 'Error: ' + err.message;
                pickerProgress.classList.remove('visible');
                console.error('handleZipFile error:', err);
            }
        }

        async function handleFolder(files) {
            var total = files.length;
            try {
                await vlfsReady;
                var xp3Paths = [];
                var folderPrefix = '/' + files[0].webkitRelativePath.split('/')[0];

                for (var i = 0; i < total; i++) {
                    var file = files[i];
                    var fullPath = '/' + file.webkitRelativePath;
                    var innerPath = fullPath.substring(folderPrefix.length);
                    var lowerName = file.name.toLowerCase();

                    setPickerProgress(Math.round(i / total * 100),
                        'Indexing (' + (i + 1) + '/' + total + ') ' + file.name);

                    // File 即 Blob：零拷贝注册
                    VLFS.registerBlobFile(innerPath, file);
                    if (lowerName.endsWith('.xp3')) xp3Paths.push(innerPath);
                }

                checkAndStartGame(xp3Paths, '');
            } catch (err) {
                pickerStatus.textContent = 'Error: ' + err.message;
                pickerProgress.classList.remove('visible');
            }
        }

        // --- Button bindings ---
        document.getElementById('pick-dir-api-btn').addEventListener('click', handleDirectoryAPI);

        document.getElementById('pick-file-btn').addEventListener('click', function() {
            ensureSpaceThen(function() {
                document.getElementById('file-input').click();
            });
        });
        document.getElementById('pick-dir-btn').addEventListener('click', function() {
            ensureSpaceThen(function() {
                document.getElementById('dir-input').click();
            });
        });
        document.getElementById('pick-zip-btn').addEventListener('click', function() {
            ensureSpaceThen(function() {
                document.getElementById('zip-input').click();
            });
        });

        document.getElementById('file-input').addEventListener('change', function(e) {
            var file = e.target.files[0];
            if (file) handleXp3File(file);
            e.target.value = '';
        });

        document.getElementById('dir-input').addEventListener('change', function(e) {
            var files = Array.from(e.target.files);
            if (files.length > 0) handleFolder(files);
            e.target.value = '';
        });

        document.getElementById('zip-input').addEventListener('change', function(e) {
            var file = e.target.files[0];
            if (file) handleZipFile(file);
            e.target.value = '';
        });

        // Drag & drop
        var dropZone = document.getElementById('drop-zone');
        dropZone.addEventListener('click', function() {
            ensureSpaceThen(function() {
                document.getElementById('file-input').click();
            });
        });
        dropZone.addEventListener('dragover', function(e) {
            e.preventDefault();
            dropZone.classList.add('dragover');
        });
        dropZone.addEventListener('dragleave', function() {
            dropZone.classList.remove('dragover');
        });
        dropZone.addEventListener('drop', function(e) {
            e.preventDefault();
            dropZone.classList.remove('dragover');
            var file = e.dataTransfer.files[0];
            if (file) {
                ensureSpaceThen(function() { handleXp3File(file); });
            }
        });
