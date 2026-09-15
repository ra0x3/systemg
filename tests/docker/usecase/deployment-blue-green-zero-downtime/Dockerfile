FROM sysg-usecase-base

RUN apt-get update \
 && apt-get install -y --no-install-recommends nginx \
 && rm -rf /var/lib/apt/lists/*

COPY tests/docker/usecase/deployment-blue-green-zero-downtime/app.py /usecase/app.py
COPY tests/docker/usecase/deployment-blue-green-zero-downtime/pin.py /usecase/pin.py
COPY tests/docker/usecase/deployment-blue-green-zero-downtime/loadgen.py /usecase/loadgen.py
COPY tests/docker/usecase/deployment-blue-green-zero-downtime/analyze.py /usecase/analyze.py
COPY tests/docker/usecase/deployment-blue-green-zero-downtime/nginx.conf /usecase/nginx.conf
COPY tests/docker/usecase/deployment-blue-green-zero-downtime/switch.sh /usecase/switch.sh
COPY tests/docker/usecase/deployment-blue-green-zero-downtime/verify.sh /usecase/verify.sh
COPY tests/docker/usecase/deployment-blue-green-zero-downtime/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/deployment-blue-green-zero-downtime/run.sh /usecase/run.sh
RUN chmod +x /usecase/switch.sh /usecase/verify.sh /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
