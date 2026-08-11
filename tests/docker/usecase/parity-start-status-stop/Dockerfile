FROM sysg-usecase-base

COPY tests/docker/usecase/parity-lib.sh /usecase/parity-lib.sh
COPY tests/docker/usecase/parity-start-status-stop/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/parity-start-status-stop/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
