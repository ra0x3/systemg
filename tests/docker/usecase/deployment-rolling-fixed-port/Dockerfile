FROM sysg-usecase-base

COPY tests/docker/usecase/deployment-rolling-fixed-port/server.py /usecase/server.py
COPY tests/docker/usecase/deployment-rolling-fixed-port/pre.sh /usecase/pre.sh
COPY tests/docker/usecase/deployment-rolling-fixed-port/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/deployment-rolling-fixed-port/run.sh /usecase/run.sh
RUN chmod +x /usecase/pre.sh /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
