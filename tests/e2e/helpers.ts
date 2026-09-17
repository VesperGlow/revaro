import { expect, type Page } from '@playwright/test'

export async function login(page:Page){
  await page.goto('/')
  await page.getByLabel('用户名').fill(process.env.E2E_USERNAME||'admin')
  await page.getByLabel('密码').fill(process.env.E2E_PASSWORD||'revaro-e2e-password')
  await page.getByRole('button',{name:'进入我的网盘'}).click()
  await expect(page.getByRole('heading',{name:'我的文件'})).toBeVisible()
}

export async function selectCard(page:Page,name:string){
  const card=page.locator('.file-card').filter({hasText:name})
  await expect(card).toBeVisible()
  await card.getByRole('button',{name:'选择项目'}).click()
}

// The reference app still exposes a grid/list switch, while the Rust app only
// renders the grid. Detect the reference switch so dual-version specs can enter
// through each app's own listing layout instead of assuming a shared one.
export async function useReferenceListView(page:Page){
  const toggle=page.getByTitle('列表视图')
  if(await toggle.count()){
    await toggle.click()
    return true
  }
  return false
}

export function listingEntries(page:Page,useListView:boolean){
  return page.locator(useListView ? '.file-row' : '.file-card')
}
