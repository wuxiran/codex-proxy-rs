FROM node:22-alpine
WORKDIR /app
COPY index.js ./
ENV NODE_ENV=production
# 89 上只绑回环：本服务转发 Authorization，等于凭据代理，不能暴露到公网。
ENV RELAY_BIND=127.0.0.1
ENV FC_SERVER_PORT=9000
USER node
CMD ["node", "index.js"]
