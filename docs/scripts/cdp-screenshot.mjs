#!/usr/bin/env node
// Minimal CDP screenshot helper: logs in via the console API, then captures pages.
import { writeFileSync } from 'node:fs'

const base = process.env.DOK_BASE || 'http://127.0.0.1:11211'
const password = process.env.DOK_PASSWORD || 'testpass'
const outDir = process.env.DOK_OUT || '/tmp'
const pages = (process.env.DOK_PAGES || 'dashboard,topology,settings,links,nodes,routes,history').split(',')
const bust = Date.now()

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

async function cdpTargets() {
  const res = await fetch('http://127.0.0.1:9222/json')
  return res.json()
}

async function connect(wsUrl) {
  const ws = new WebSocket(wsUrl)
  await new Promise((resolve, reject) => {
    ws.onopen = resolve
    ws.onerror = reject
  })
  let id = 0
  const pending = new Map()
  ws.onmessage = (ev) => {
    const msg = JSON.parse(ev.data)
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id)
      pending.delete(msg.id)
      msg.error ? reject(new Error(JSON.stringify(msg.error))) : resolve(msg.result)
    }
  }
  const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const msgId = ++id
      pending.set(msgId, { resolve, reject })
      ws.send(JSON.stringify({ id: msgId, method, params }))
    })
  return { ws, send }
}

async function main() {
  const targets = await cdpTargets()
  const target = targets.find((t) => t.type === 'page')
  if (!target) throw new Error('no page target')
  const { ws, send } = await connect(target.webSocketDebuggerUrl)

  await send('Page.enable')
  await send('Network.enable')
  await send('Network.setCacheDisabled', { cacheDisabled: true })
  await send('Runtime.enable')
  ws.addEventListener('message', (ev) => {
    const msg = JSON.parse(ev.data)
    if (msg.method === 'Runtime.exceptionThrown') console.log('EXC', JSON.stringify(msg.params.exceptionDetails).slice(0, 500))
    if (msg.method === 'Runtime.consoleAPICalled' && ['error','warning'].includes(msg.params.type)) console.log(msg.params.type.toUpperCase(), JSON.stringify(msg.params.args).slice(0, 600))
  })
  await send('Emulation.setDeviceMetricsOverride', {
    width: 1440,
    height: 900,
    deviceScaleFactor: 1,
    mobile: false,
  })

  await send('Page.addScriptToEvaluateOnNewDocument', {
    source: "window.__errs=[];addEventListener('error',e=>__errs.push(String(e.error||e.message)));addEventListener('unhandledrejection',e=>__errs.push(String(e.reason)));",
  })
  await send('Page.navigate', { url: `${base}/?v=${bust}#/login` })
  await sleep(1500)
  await send('Runtime.evaluate', {
    expression: `fetch('/api/login',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({password:${JSON.stringify(password)}})}).then(r=>r.status)`,
    awaitPromise: true,
  })
  await sleep(500)

  for (const page of pages) {
    await send('Page.navigate', { url: `${base}/?v=${bust}#/${page}` })
    await sleep(2200)
    if (process.env.DOK_CLICKS) {
      for (const pair of process.env.DOK_CLICKS.split(';')) {
        const [cx, cy] = pair.split(',').map(Number)
        await send('Input.dispatchMouseEvent', { type: 'mousePressed', x: cx, y: cy, button: 'left', clickCount: 1 })
        await sleep(60)
        await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: cx, y: cy, button: 'left', clickCount: 1 })
        await sleep(500)
      }
    }
    if (process.env.DOK_CLICK) {
      const [cx, cy] = process.env.DOK_CLICK.split(',').map(Number)
      await send('Input.dispatchMouseEvent', { type: 'mousePressed', x: cx, y: cy, button: 'left', clickCount: 1 })
      await sleep(80)
      await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: cx, y: cy, button: 'left', clickCount: 1 })
      await sleep(700)
    }
    if (process.env.DOK_DRAG === 'auto') {
      const res = await send('Runtime.evaluate', {
        expression: 'JSON.stringify(Object.entries(window.__dokLayout||{}))',
        returnByValue: true,
      })
      const rectRes = await send('Runtime.evaluate', {
        expression: 'JSON.stringify(window.__dokRect())',
        returnByValue: true,
      })
      const rect = JSON.parse(rectRes.result.value || '{"left":0,"top":0}')
      const entries = JSON.parse(res.result.value || '[]')
      if (entries.length) {
        const [id, p] = entries[0]
        const before = p.x + ',' + p.y
        const x = rect.left + p.x
        const y = rect.top + p.y
        await send('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'left', clickCount: 1 })
        await sleep(100)
        await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: x + 90, y: y + 70, button: 'left', buttons: 1 })
        await sleep(100)
        await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: x + 90, y: y + 70, button: 'left', clickCount: 1 })
        await sleep(250)
        const after = await send('Runtime.evaluate', { expression: 'JSON.stringify(window.__dokLayout)', returnByValue: true })
        console.log('DRAG', id, before, '->', after.result.value)
      }
    } else if (process.env.DOK_DRAG) {
      const [x1, y1, x2, y2] = process.env.DOK_DRAG.split(',').map(Number)
      await send('Input.dispatchMouseEvent', { type: 'mousePressed', x: x1, y: y1, button: 'left', clickCount: 1 })
      await sleep(120)
      await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: (x1 + x2) / 2, y: (y1 + y2) / 2, button: 'left', buttons: 1 })
      await sleep(120)
      await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: x2, y: y2, button: 'left', buttons: 1 })
      await sleep(120)
      await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: x2, y: y2, button: 'left', clickCount: 1 })
      await sleep(300)
    }
    const dom = await send('Runtime.evaluate', {
      expression: "JSON.stringify({res:performance.getEntriesByType('resource').map(r=>r.name.split('/').pop()).filter(n=>n.includes('HistoryView')||n.includes('index-')), scripts:[...document.querySelectorAll('script')].map(s=>s.src.split('/').pop())})",
      returnByValue: true,
    })
    console.log(page, dom.result.value)
    const errs = await send('Runtime.evaluate', { expression: 'JSON.stringify(window.__errs||[])', returnByValue: true })
    if (errs.result.value !== '[]') console.log('ERRORS', errs.result.value)
    const shot = await send('Page.captureScreenshot', { format: 'png' })
    const path = `${outDir}/dok-${page}.png`
    writeFileSync(path, Buffer.from(shot.data, 'base64'))
    console.log(path)
  }
  ws.close()
}

main().catch((err) => {
  console.error(err)
  process.exit(1)
})
