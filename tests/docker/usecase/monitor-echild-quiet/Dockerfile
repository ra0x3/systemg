FROM sysg-usecase-base

COPY tests/docker/usecase/monitor-echild-quiet/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/monitor-echild-quiet/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
