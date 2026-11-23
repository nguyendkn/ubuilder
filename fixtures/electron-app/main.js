const { app, BrowserWindow } = require("electron");

function createWindow() {
  const window = new BrowserWindow({ width: 800, height: 600 });
  window.loadFile("index.html");
}

app.whenReady().then(createWindow);
app.on("window-all-closed", () => {
  if (process.platform !== "darwin") app.quit();
});
