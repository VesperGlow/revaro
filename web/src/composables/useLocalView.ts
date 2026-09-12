import { ref, watch, type Ref } from 'vue'

// 视图偏好（方块/列表、全部/分类）在本地持久化，切换分类后仍然保留。
export function usePersistentMode<T extends string>(key: string, fallback: T, allowed: readonly T[]): Ref<T> {
  let initial = fallback
  try {
    const raw = localStorage.getItem(key)
    if (raw && (allowed as readonly string[]).includes(raw)) initial = raw as T
  } catch {
    // 隐私模式下 localStorage 可能不可用，回退到默认值。
  }
  const mode = ref(initial) as Ref<T>
  watch(mode, value => {
    try {
      localStorage.setItem(key, value)
    } catch {
      // 忽略写入失败。
    }
  })
  return mode
}
