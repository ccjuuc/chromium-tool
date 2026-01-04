# 请求 Pending 问题分析

## 问题现象
从 DevTools 网络面板看到：
- 两个 `build_package` 请求都处于 `(pending)` 状态
- 一个是正常的 POST 请求（Initiator: `injected.js:1`）
- 另一个是 Preflight 请求（Initiator: `Preflight`）

## 可能的原因

### 1. ⚠️ **CORS 预检请求未正确处理**
**问题：**
- 浏览器发送 OPTIONS 预检请求，但服务器可能没有正确响应
- `CorsLayer` 配置可能不完整

**当前配置：**
```rust
let cors = CorsLayer::new()
    .allow_origin(Any)
    .allow_methods(Any)
    .allow_headers(Any);
```

**修复：**
- 明确指定允许的方法（GET, POST, OPTIONS）
- 添加 `max_age` 缓存预检响应
- 确保 CORS 层在所有路由之后应用

### 2. ⚠️ **并发限制导致阻塞**
**问题：**
```rust
.route("/build_package", post(handlers::build::build_package))
.layer(ConcurrencyLimitLayer::new(1))  // 限制并发为 1
```

**影响：**
- 如果已经有请求在处理，新请求会 pending
- Preflight 请求也会被限制，导致阻塞

**修复：**
- OPTIONS 请求不应该被并发限制
- 或者将并发限制只应用到实际构建逻辑，而不是整个路由

### 3. ⚠️ **服务器地址配置错误**
**问题：**
前端代码：
```javascript
const serverUrl = `http://${server}`;
const response = await fetch(`${serverUrl}/build_package`, {
```

**影响：**
- 如果 `server` 变量配置错误，请求会发送到错误的地址
- 目标服务器可能不存在或未运行

**检查：**
- 确认 `server` 变量的值
- 确认目标服务器是否运行
- 检查网络连接

### 4. ⚠️ **服务器未运行或崩溃**
**问题：**
- 服务器可能没有启动
- 服务器可能崩溃了
- 端口可能被占用

**检查：**
```bash
# 检查进程
ps aux | grep chromium_tool

# 检查端口
lsof -i :3000

# 检查服务器日志
```

### 5. ⚠️ **Handler 执行时间过长**
**问题：**
`build_package` handler 可能：
- 执行了长时间运行的同步操作
- 等待数据库操作超时
- 等待文件系统操作

**检查：**
- 查看服务器日志
- 检查是否有错误信息
- 确认 handler 是否真的在执行

## 修复方案

### 方案 1: 完善 CORS 配置（已实施）
```rust
let cors = CorsLayer::new()
    .allow_origin(Any)
    .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
    .allow_headers(Any)
    .allow_credentials(false)
    .expose_headers(Any)
    .max_age(std::time::Duration::from_secs(3600));
```

### 方案 2: 调整并发限制策略
- 将并发限制只应用到实际构建任务，而不是路由层
- 或者排除 OPTIONS 请求

### 方案 3: 添加超时处理
- 为请求添加超时
- 添加更好的错误处理

### 方案 4: 添加日志和监控
- 记录所有请求
- 记录处理时间
- 记录错误信息

## 调试步骤

1. **检查服务器是否运行**
   ```bash
   curl http://127.0.0.1:3000/server_list
   ```

2. **检查 CORS 预检请求**
   ```bash
   curl -X OPTIONS http://127.0.0.1:3000/build_package \
     -H "Origin: http://127.0.0.1:3000" \
     -H "Access-Control-Request-Method: POST" \
     -v
   ```

3. **检查实际请求**
   ```bash
   curl -X POST http://127.0.0.1:3000/build_package \
     -H "Content-Type: application/json" \
     -d '{"branch":"main","platform":"linux"}' \
     -v
   ```

4. **查看服务器日志**
   - 检查是否有错误信息
   - 检查请求是否到达服务器
   - 检查处理时间

## 建议的修复优先级

1. ✅ **高优先级**：完善 CORS 配置（已修复）
2. ⚠️ **高优先级**：检查服务器是否运行
3. ⚠️ **中优先级**：调整并发限制策略
4. ⚠️ **中优先级**：添加请求超时
5. ⚠️ **低优先级**：添加更详细的日志

