FROM sysg-usecase-base

COPY tests/docker/usecase/deployment-recovery-completed-dependency/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/deployment-recovery-completed-dependency/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
