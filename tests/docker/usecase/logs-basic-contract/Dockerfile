FROM sysg-usecase-base

COPY tests/docker/usecase/logs-basic-contract/emitter.py /usecase/emitter.py
COPY tests/docker/usecase/logs-basic-contract/pty_check.py /usecase/pty_check.py
COPY tests/docker/usecase/logs-basic-contract/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/logs-basic-contract/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
