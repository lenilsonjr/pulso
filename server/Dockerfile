FROM python:3.12-slim
WORKDIR /app
COPY server.py .
ENV PULSO_DATA=/data PULSO_PORT=8787 PULSO_BIND=0.0.0.0
VOLUME /data
EXPOSE 8787
CMD ["python", "server.py"]
