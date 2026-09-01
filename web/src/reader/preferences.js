export function createReaderPreferences(getElements,relayout){
  function toggleTools(){const els=getElements();els.themeRoot.classList.toggle('tools-hidden');if(els.themeRoot.classList.contains('tools-hidden'))els.fontPopover?.classList.add('hidden')}
  function toggleTheme(){const els=getElements();els.themeRoot.classList.toggle('dark');try{localStorage.setItem('reader-theme',els.themeRoot.classList.contains('dark')?'dark':'light')}catch(_){}}
  function setReaderFontSize(size){const els=getElements(),pixels=Math.max(14,Math.min(32,Number(size)||18));document.documentElement.style.setProperty('--reader-font-size',`${pixels}px`);if(els.fontSlider)els.fontSlider.value=pixels;try{localStorage.setItem('reader-font-size',pixels)}catch(_){}relayout()}
  function stepFontSize(delta){const current=parseInt(getComputedStyle(document.documentElement).getPropertyValue('--reader-font-size'),10)||19;setReaderFontSize(current+delta)}
  function loadPrefs(){const els=getElements();try{const size=Number(localStorage.getItem('reader-font-size'));if(Number.isFinite(size)&&size>0){const pixels=Math.max(14,Math.min(32,size));document.documentElement.style.setProperty('--reader-font-size',`${pixels}px`);if(els.fontSlider)els.fontSlider.value=pixels}const theme=localStorage.getItem('reader-theme');if(theme==='dark')els.themeRoot.classList.add('dark');else if(theme==='light')els.themeRoot.classList.remove('dark')}catch(_){}}
  return {toggleTools,toggleTheme,setReaderFontSize,stepFontSize,loadPrefs}
}
