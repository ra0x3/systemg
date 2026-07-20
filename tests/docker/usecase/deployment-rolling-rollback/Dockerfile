FROM sysg-usecase-base

COPY tests/docker/usecase/deployment-rolling-rollback/server.py /usecase/server.py
COPY tests/docker/usecase/deployment-rolling-rollback/pre.sh /usecase/pre.sh
COPY tests/docker/usecase/deployment-rolling-rollback/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/deployment-rolling-rollback/run.sh /usecase/run.sh
RUN chmod +x /usecase/pre.sh /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
