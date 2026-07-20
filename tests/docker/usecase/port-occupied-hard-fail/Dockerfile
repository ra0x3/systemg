FROM sysg-usecase-base

COPY tests/docker/usecase/port-occupied-hard-fail/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/port-occupied-hard-fail/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
