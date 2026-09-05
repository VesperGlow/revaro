// ESLint 9+ flat config：TypeScript + Vue3。
// 风格取向宽松（本项目为紧凑无分号风格），重点抓真实问题：
// 未使用变量、明显的错误模式、模板安全等。
import js from '@eslint/js'
import globals from 'globals'
import tseslint from 'typescript-eslint'
import pluginVue from 'eslint-plugin-vue'

export default tseslint.config(
  { ignores: ['dist/**', 'node_modules/**', '.npm-cache/**', 'playwright-report/**', 'test-results/**'] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  ...pluginVue.configs['flat/recommended'],
  {
    files: ['**/*.{js,ts,vue}'],
    languageOptions: {
      globals: { ...globals.browser },
      parserOptions: { parser: tseslint.parser, extraFileExtensions: ['.vue'] },
    },
    rules: {
      // v-html 仅用于经过 DOMPurify 消毒的 Markdown 预览
      'vue/no-v-html': 'off',
      // 组件名单文件（App.vue 等）是项目惯例
      'vue/multi-word-component-names': 'off',
      // 紧凑单行风格，关闭格式类噪音
      'vue/max-attributes-per-line': 'off',
      'vue/html-self-closing': 'off',
      'vue/singleline-html-element-content-newline': 'off',
      'vue/html-closing-bracket-spacing': 'off',
      'vue/attributes-order': 'off',
      '@typescript-eslint/no-explicit-any': 'error',
      // 保留项目内用于条件清理和可选调用的短路表达式。
      '@typescript-eslint/no-unused-expressions': ['error', { allowShortCircuit: true }],
      // 空 catch 用于忽略 localStorage 在隐私模式下的异常
      'no-empty': ['error', { allowEmptyCatch: true }],
      'no-unused-vars': 'off',
      '@typescript-eslint/no-unused-vars': ['warn', { argsIgnorePattern: '^_', varsIgnorePattern: '^_', caughtErrorsIgnorePattern: '^_' }],
    },
  },
)
