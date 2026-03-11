FROM node:20-alpine

RUN npm install -g @fission-ai/openspec

WORKDIR /app
COPY package.json ./
RUN npm install --omit=dev

COPY server.js ./

RUN mkdir -p /workspace

EXPOSE 3000

CMD ["node", "server.js"]
